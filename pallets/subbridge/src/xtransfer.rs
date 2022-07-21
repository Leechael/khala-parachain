pub use self::pallet::*;

#[frame_support::pallet]
pub mod pallet {
	use crate::helper::WrapSlice;
	use crate::traits::*;
	use assets_registry::AccountId32Conversion;
	use frame_support::{
		dispatch::DispatchResult, pallet_prelude::*, traits::StorageVersion, transactional,
		weights::Weight,
	};
	use frame_system::pallet_prelude::*;
	use sp_std::{boxed::Box, convert::From, vec::Vec};
	use xcm::latest::{prelude::*, MultiAsset, MultiLocation};

	const STORAGE_VERSION: StorageVersion = StorageVersion::new(3);

	#[pallet::pallet]
	#[pallet::generate_store(pub(super) trait Store)]
	#[pallet::storage_version(STORAGE_VERSION)]
	pub struct Pallet<T>(_);

	#[pallet::config]
	pub trait Config: frame_system::Config {
		type Event: From<Event<Self>> + IsType<<Self as frame_system::Config>::Event>;
		type Bridge: BridgeTransact;
	}

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		/// Assets being withdrawn from somewhere.
		Withdrawn {
			what: MultiAsset,
			who: MultiLocation,
			memo: Vec<u8>,
		},
		/// Assets being deposited to somewhere.
		Deposited {
			what: MultiAsset,
			who: MultiLocation,
			memo: Vec<u8>,
		},
		/// Assets being forwarded to somewhere.
		Forwarded {
			what: MultiAsset,
			who: MultiLocation,
			memo: Vec<u8>,
		},
	}

	#[pallet::error]
	pub enum Error<T> {
		_TransactFailed,
		UnknownAsset,
		UnsupportedDest,
		UnhandledTransfer,
	}

	#[pallet::call]
	impl<T: Config> Pallet<T>
	where
		T::AccountId: Into<[u8; 32]> + From<[u8; 32]>,
	{
		#[pallet::weight(195_000_000)]
		#[transactional]
		pub fn transfer(
			origin: OriginFor<T>,
			asset: Box<MultiAsset>,
			dest: Box<MultiLocation>,
			dest_weight: Option<Weight>,
		) -> DispatchResult {
			let sender = ensure_signed(origin)?;
			Self::do_transfer(sender, *asset, *dest, dest_weight)?;
			Ok(())
		}

		#[pallet::weight(195_000_000)]
		#[transactional]
		pub fn transfer_generic(
			origin: OriginFor<T>,
			data: Box<Vec<u8>>,
			dest: Box<MultiLocation>,
			dest_weight: Option<Weight>,
		) -> DispatchResult {
			let sender = ensure_signed(origin)?;
			Self::do_transfer_generic(sender, data.to_vec(), *dest, dest_weight)?;
			Ok(())
		}
	}

	impl<T: Config> Pallet<T>
	where
		T::AccountId: Into<[u8; 32]> + From<[u8; 32]>,
	{
		fn do_transfer(
			sender: T::AccountId,
			what: MultiAsset,
			dest: MultiLocation,
			dest_weight: Option<Weight>,
		) -> DispatchResult {
			let bridge = T::Bridge::new();
			match (&what.fun, &what.id) {
				// Fungible assets
				(Fungible(_), Concrete(_)) => {
					bridge.transfer_fungible(sender.into(), what, dest, dest_weight)?;
				}
				// NonFungible assets
				(NonFungible(_), Concrete(_)) => {
					bridge.transfer_nonfungible(sender.into(), what, dest, dest_weight)?;
				}
				_ => return Err(Error::<T>::UnknownAsset.into()),
			}
			Ok(())
		}

		fn do_transfer_generic(
			sender: T::AccountId,
			data: Vec<u8>,
			dest: MultiLocation,
			dest_weight: Option<Weight>,
		) -> DispatchResult {
			let bridge = T::Bridge::new();
			bridge.transfer_generic(sender.into(), &data, dest, dest_weight)?;
			Ok(())
		}
	}

	impl<T: Config> OnWithdrawn for Pallet<T> {
		fn on_withdrawn(what: MultiAsset, who: MultiLocation, memo: Vec<u8>) -> DispatchResult {
			Self::deposit_event(Event::Withdrawn { what, who, memo });
			Ok(())
		}
	}

	impl<T: Config> OnDeposited for Pallet<T> {
		fn on_deposited(what: MultiAsset, who: MultiLocation, memo: Vec<u8>) -> DispatchResult {
			Self::deposit_event(Event::Deposited { what, who, memo });
			Ok(())
		}
	}

	impl<T: Config> OnForwarded for Pallet<T>
	where
		<T as frame_system::Config>::AccountId: From<[u8; 32]> + Into<[u8; 32]>,
	{
		fn on_forwarded(what: MultiAsset, who: MultiLocation, memo: Vec<u8>) -> DispatchResult {
			// Every forwarded transfer will deposit asset into temporary account in advance, so here we
			// use it as sender, asset will be withdrawn from this account.
			// TODO: Handle the sitution when forwarding failed. Maybe need to have something like `AssesTrap`
			// and `AssetsClaim`.
			let temporary_account =
				MultiLocation::new(0, X1(GeneralKey(WrapSlice(b"bridge_transfer").into())))
					.into_account();
			Self::do_transfer(
				temporary_account.into(),
				what.clone(),
				who.clone(),
				6_000_000_000u64.into(),
			)?;
			Self::deposit_event(Event::Forwarded { what, who, memo });
			// TODO: Should we support forward generic message in the future?
			Ok(())
		}
	}

	#[cfg(test)]
	mod test {
		use crate::chainbridge::Error as ChainbridgeError;
		use crate::chainbridge::Event as ChainbridgeEvent;
		use crate::helper::WrapSlice;
		use crate::mock::para::Origin;
		use crate::mock::para::Runtime;
		use crate::mock::{
			para, para_expect_event, ParaA, ParaAssets as Assets,
			ParaAssetsRegistry as AssetsRegistry, ParaB, ParaBalances,
			ParaChainBridge as ChainBridge, ParaResourceIdGenSalt, ParaXTransfer as XTransfer,
			TestNet, ALICE, BOB, ENDOWED_BALANCE,
		};
		use crate::traits::*;
		use frame_support::{assert_noop, assert_ok};
		use polkadot_parachain::primitives::Sibling;
		use sp_runtime::{traits::AccountIdConversion, AccountId32};

		use assets_registry::{
			AccountId32Conversion, AssetProperties, ExtractReserveLocation, IntoResourceId,
			ASSETS_REGISTRY_ID,
		};
		use xcm::latest::{prelude::*, MultiLocation};
		use xcm_simulator::TestExt;

		fn sibling_account(para_id: u32) -> AccountId32 {
			Sibling::from(para_id).into_account_truncating()
		}

		#[test]
		fn test_transfer_unregistered_assets_to_parachain_should_failed() {
			TestNet::reset();

			let unregistered_asset_location =
				MultiLocation::new(0, X1(GeneralKey(WrapSlice(b"unregistered").into())));

			ParaA::execute_with(|| {
				// To parachains via Xcm(according to the dest)
				assert_noop!(
					XTransfer::transfer(
						Origin::signed(ALICE),
						Box::new(
							(
								Concrete(unregistered_asset_location.clone()),
								Fungible(100u128)
							)
								.into()
						),
						Box::new(MultiLocation::new(1, X1(Parachain(2)))),
						Some(6_000_000_000u64),
					),
					// Both XcmBridge and ChainBridge will failed with "CannotDepositAsset", however XcmBridge
					// will run first, then ChainBridge will run according to our mock runtime definition.
					// And we always return the last error when iterating all configured bridges.
					ChainbridgeError::<Runtime>::CannotDepositAsset,
				);
			});
		}

		#[test]
		fn test_transfer_unregistered_assets_to_solochain_should_failed() {
			TestNet::reset();

			let unregistered_asset_location =
				MultiLocation::new(0, X1(GeneralKey(WrapSlice(b"unregistered").into())));

			ParaA::execute_with(|| {
				// To solo chains via Chainbridge(according to the dest)
				assert_noop!(
					XTransfer::transfer(
						Origin::signed(ALICE),
						Box::new((Concrete(unregistered_asset_location), Fungible(100u128)).into()),
						Box::new(MultiLocation::new(
							0,
							X3(
								GeneralKey(WrapSlice(b"cb").into()),
								GeneralIndex(0),
								GeneralKey(WrapSlice(b"recipient").into())
							)
						)),
						None,
					),
					ChainbridgeError::<Runtime>::CannotDepositAsset,
				);
			});
		}

		#[test]
		fn test_transfer_by_chainbridge_without_enabled_should_failed() {
			TestNet::reset();

			let registered_asset_location =
				MultiLocation::new(0, X1(GeneralKey(WrapSlice(b"registered").into())));
			ParaA::execute_with(|| {
				// Register asset
				assert_ok!(AssetsRegistry::force_register_asset(
					para::Origin::root(),
					registered_asset_location.clone().into(),
					0,
					AssetProperties {
						name: b"RegisteredAsset".to_vec(),
						symbol: b"RA".to_vec(),
						decimals: 12,
					},
				));

				// To solochains via Chainbridge(according to the dest)
				assert_noop!(
					XTransfer::transfer(
						Origin::signed(ALICE),
						Box::new((Concrete(registered_asset_location), Fungible(100u128)).into()),
						Box::new(MultiLocation::new(
							0,
							X3(
								GeneralKey(WrapSlice(b"cb").into()),
								GeneralIndex(0),
								GeneralKey(WrapSlice(b"recipient").into())
							)
						)),
						None,
					),
					ChainbridgeError::<Runtime>::CannotDepositAsset,
				);
			});
		}

		#[test]
		fn test_transfer_by_chainbridge_without_feeset_should_failed() {
			TestNet::reset();

			let registered_asset_location =
				MultiLocation::new(0, X1(GeneralKey(WrapSlice(b"registered").into())));
			ParaA::execute_with(|| {
				// Register asset
				assert_ok!(AssetsRegistry::force_register_asset(
					para::Origin::root(),
					registered_asset_location.clone().into(),
					0,
					AssetProperties {
						name: b"RegisteredAsset".to_vec(),
						symbol: b"RA".to_vec(),
						decimals: 12,
					},
				));

				// Enable Chainbridge bridge for the asset
				assert_ok!(AssetsRegistry::force_enable_chainbridge(
					para::Origin::root(),
					0, // asset id
					0, // chain id
					true,
					Box::new(Vec::new()),
				));

				// To solochains via Chainbridge(according to the dest)
				assert_noop!(
					XTransfer::transfer(
						Origin::signed(ALICE),
						Box::new((Concrete(registered_asset_location), Fungible(100u128)).into()),
						Box::new(MultiLocation::new(
							0,
							X3(
								GeneralKey(WrapSlice(b"cb").into()),
								GeneralIndex(0),
								GeneralKey(WrapSlice(b"recipient").into())
							)
						)),
						None,
					),
					ChainbridgeError::<Runtime>::CannotDepositAsset,
				);
			});
		}

		#[test]
		fn test_transfer_assets_to_local_should_failed() {
			TestNet::reset();

			let registered_asset_location =
				MultiLocation::new(0, X1(GeneralKey(WrapSlice(b"registered").into())));
			ParaA::execute_with(|| {
				// Register asset
				assert_ok!(AssetsRegistry::force_register_asset(
					para::Origin::root(),
					registered_asset_location.clone().into(),
					0,
					AssetProperties {
						name: b"RegisteredAsset".to_vec(),
						symbol: b"RA".to_vec(),
						decimals: 12,
					},
				));

				// To solochains via Chainbridge(according to the dest)
				assert_noop!(
					XTransfer::transfer(
						Origin::signed(ALICE),
						Box::new((Concrete(registered_asset_location), Fungible(100u128)).into()),
						Box::new(MultiLocation::new(
							0,
							X1(Junction::AccountId32 {
								network: NetworkId::Any,
								id: ALICE.into(),
							})
						)),
						None,
					),
					// Both XcmBridge and ChainBridge will failed with "CannotDepositAsset", however XcmBridge
					// will run first, then ChainBridge will run according to our mock runtime definition.
					// And we always return the last error when iterating all configured bridges.
					ChainbridgeError::<Runtime>::CannotDepositAsset,
				);
			});
		}

		#[test]
		fn test_transfer_pha_to_solochain_by_chainbridge() {
			TestNet::reset();

			let pha_location = MultiLocation::new(0, Here);

			ParaA::execute_with(|| {
				// Set bridge fee and whitelist chain for the dest chain
				assert_ok!(ChainBridge::whitelist_chain(Origin::root(), 0));
				assert_ok!(ChainBridge::update_fee(Origin::root(), 2, 0));

				// To solochains via Chainbridge(according to the dest)
				assert_ok!(XTransfer::transfer(
					Origin::signed(ALICE),
					Box::new((Concrete(pha_location.clone()), Fungible(100u128)).into()),
					Box::new(MultiLocation::new(
						0,
						X3(
							GeneralKey(WrapSlice(b"cb").into()),
							GeneralIndex(0),
							GeneralKey(WrapSlice(b"recipient").into())
						)
					)),
					None,
				));

				para_expect_event(ChainbridgeEvent::FungibleTransfer(
					0, // dest chain
					1, // deposit nonce
					IntoResourceId::<ParaResourceIdGenSalt>::into_rid(pha_location, 0),
					98u128.into(), // deducted fee: 2
					b"recipient".to_vec(),
				));

				assert_eq!(ParaBalances::free_balance(&ALICE), ENDOWED_BALANCE - 100);
				assert_eq!(ParaBalances::free_balance(&para::TREASURY::get()), 2);
				assert_eq!(
					ParaBalances::free_balance(&ChainBridge::account_id()),
					ENDOWED_BALANCE + 100 - 2
				);
			});
		}

		#[test]
		fn test_transfer_asset_to_solochain_by_chainbridge() {
			TestNet::reset();

			let registered_asset_location = para::SoloChain2AssetLocation::get();
			let dest = MultiLocation::new(
				0,
				X3(
					GeneralKey(WrapSlice(b"cb").into()),
					GeneralIndex(0),
					GeneralKey(WrapSlice(b"recipient").into()),
				),
			);

			ParaA::execute_with(|| {
				// Register asset
				assert_ok!(AssetsRegistry::force_register_asset(
					para::Origin::root(),
					registered_asset_location.clone().into(),
					0,
					AssetProperties {
						name: b"RegisteredAsset".to_vec(),
						symbol: b"RA".to_vec(),
						decimals: 12,
					},
				));
				// Set execution price of asset, price is 2 * NativeExecutionPrice * 10^(12 - 12)
				assert_ok!(AssetsRegistry::force_set_price(
					Origin::root(),
					0,
					para::NativeExecutionPrice::get() * 2,
				));

				// Enable Chainbridge bridge for the asset
				assert_ok!(AssetsRegistry::force_enable_chainbridge(
					para::Origin::root(),
					0, // asset id
					0, // chain id
					true,
					Box::new(Vec::new()),
				));

				// Mint some token to ALICE
				assert_ok!(Assets::mint(
					Origin::signed(ASSETS_REGISTRY_ID.into_account_truncating()),
					0,
					ALICE,
					ENDOWED_BALANCE
				));
				assert_eq!(Assets::balance(0, &ALICE), ENDOWED_BALANCE);

				// Set bridge fee and whitelist chain for the dest chain
				assert_ok!(ChainBridge::whitelist_chain(Origin::root(), 0));
				assert_ok!(ChainBridge::update_fee(Origin::root(), 2, 0));

				// To solochains via Chainbridge(according to the dest)
				assert_ok!(XTransfer::transfer(
					Origin::signed(ALICE),
					Box::new(
						(
							Concrete(registered_asset_location.clone()),
							Fungible(100u128)
						)
							.into()
					),
					Box::new(dest.clone()),
					None,
				));

				para_expect_event(ChainbridgeEvent::FungibleTransfer(
					0, // dest chain
					1, // deposit nonce
					IntoResourceId::<ParaResourceIdGenSalt>::into_rid(registered_asset_location, 0),
					96u128.into(), // deducted fee: 4
					b"recipient".to_vec(),
				));

				assert_eq!(Assets::balance(0, &ALICE), ENDOWED_BALANCE - 100);
				// Fee ratio: PHA : SoloChain2AssetLocation = 1 : 2
				assert_eq!(Assets::balance(0, &para::TREASURY::get()), 4);
				// Transfer to non-reserve dest, asset will be saved in reserved account
				assert_eq!(
					Assets::balance(0, &dest.reserve_location().unwrap().into_account().into()),
					96 // deducted fee: 4
				);
			});
		}

		#[test]
		fn test_transfer_pha_to_parachain_by_xcm() {
			TestNet::reset();

			let pha_local_location = MultiLocation::new(0, Here);
			let pha_location: MultiLocation = MultiLocation::new(1, X1(Parachain(1)));

			ParaB::execute_with(|| {
				// ParaB register the native asset of paraA, e.g. PHA here.
				assert_ok!(AssetsRegistry::force_register_asset(
					para::Origin::root(),
					pha_location.clone().into(),
					0,
					AssetProperties {
						name: b"ParaAAsset".to_vec(),
						symbol: b"PAA".to_vec(),
						decimals: 12,
					},
				));
			});

			ParaA::execute_with(|| {
				// To solochains via Chainbridge(according to the dest)
				assert_ok!(XTransfer::transfer(
					Origin::signed(ALICE),
					Box::new((Concrete(pha_local_location.clone()), Fungible(100u128)).into()),
					Box::new(MultiLocation::new(
						1,
						X2(
							Parachain(2),
							Junction::AccountId32 {
								network: NetworkId::Any,
								id: BOB.into()
							}
						)
					)),
					Some(1),
				));

				assert_eq!(ParaBalances::free_balance(&ALICE), ENDOWED_BALANCE - 100);
				// Due to transfer to non-reserve location, will save asset into sovereign account
				assert_eq!(ParaBalances::free_balance(&sibling_account(2)), 100);
			});

			ParaB::execute_with(|| {
				assert_eq!(Assets::balance(0, &BOB), 100 - 1);
			});
		}

		#[test]
		fn test_transfer_asset_to_parachain_by_xcm() {
			let para_a_location: MultiLocation = MultiLocation {
				parents: 1,
				interior: X1(Parachain(1)),
			};

			ParaB::execute_with(|| {
				// ParaB register the native asset of paraA
				assert_ok!(AssetsRegistry::force_register_asset(
					para::Origin::root(),
					para_a_location.clone().into(),
					0,
					AssetProperties {
						name: b"ParaAAsset".to_vec(),
						symbol: b"PAA".to_vec(),
						decimals: 12,
					},
				));
				// ParaB set price of the native asset of paraA
				assert_ok!(AssetsRegistry::force_set_price(para::Origin::root(), 0, 1,));
			});

			ParaA::execute_with(|| {
				let bridge_impl = crate::xcmbridge::BridgeTransactImpl::<Runtime>::new();
				// ParaA send it's own native asset to paraB
				assert_ok!(bridge_impl.transfer_fungible(
					ALICE.into(),
					(Concrete(MultiLocation::new(0, Here)), Fungible(10u128)).into(),
					MultiLocation::new(
						1,
						X2(
							Parachain(2u32.into()),
							Junction::AccountId32 {
								network: NetworkId::Any,
								id: BOB.into()
							}
						)
					),
					Some(1),
				));

				assert_eq!(ParaBalances::free_balance(&ALICE), ENDOWED_BALANCE - 10);
				assert_eq!(ParaBalances::free_balance(&sibling_account(2)), 10);
			});

			ParaB::execute_with(|| {
				assert_eq!(Assets::balance(0u32.into(), &BOB), 10 - 1);
			});

			// Now, let's transfer back to paraA use xtransfer instread of xcm bridge
			ParaB::execute_with(|| {
				// ParaB send back ParaA's native asset
				assert_ok!(XTransfer::transfer(
					Origin::signed(BOB),
					Box::new((Concrete(para_a_location.clone()), Fungible(5u128)).into()),
					Box::new(MultiLocation::new(
						1,
						X2(
							Parachain(1u32.into()),
							Junction::AccountId32 {
								network: NetworkId::Any,
								id: ALICE.into()
							}
						)
					)),
					Some(1),
				));

				assert_eq!(Assets::balance(0u32.into(), &BOB), 9 - 5);
			});

			ParaA::execute_with(|| {
				assert_eq!(ParaBalances::free_balance(&sibling_account(2)), 5);
				assert_eq!(ParaBalances::free_balance(&ALICE), ENDOWED_BALANCE - 10 + 4);
			});
		}
	}
}
