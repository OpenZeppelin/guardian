use miden_protocol::Felt;
use miden_protocol::account::{
    Account, AccountCodePatch, AccountDelta, AccountPatch, AccountStoragePatch, AccountVaultPatch,
    StorageSlotPatch,
};
use miden_protocol::asset::AssetVault;

/// Applies a partial [`AccountDelta`] to an existing account.
pub fn apply_account_delta(account: &mut Account, delta: &AccountDelta) -> Result<(), String> {
    apply_account_delta_with_storage_patch(account, delta, AccountStoragePatch::new())
}

/// Returns `true` if `delta` creates a new account: it carries account code and the account it
/// applies to has not executed a transaction yet.
///
/// Since Miden 0.17.0-rc.8 a delta also carries code when a transaction upgrades the code of an
/// existing account, so the code alone no longer identifies a creation.
pub fn is_account_creation(account: &Account, delta: &AccountDelta) -> bool {
    account.is_new() && !delta.code().is_empty()
}

/// Applies an [`AccountDelta`] and additional storage updates atomically.
///
/// The additional patch is merged into the delta's storage patch before application. This is
/// useful when transaction simulation omits deterministic storage writes that occur during
/// authentication. A delta that upgrades the account code replaces the code.
pub fn apply_account_delta_with_storage_patch(
    account: &mut Account,
    delta: &AccountDelta,
    additional_storage: AccountStoragePatch,
) -> Result<(), String> {
    if is_account_creation(account, delta) {
        return Err("cannot apply an account-creating delta to an existing account".to_string());
    }

    let rebuilds_new_account = rebuilds_new_account(account, delta);
    let patch = account_patch_from_delta(account, delta, additional_storage)?;
    if rebuilds_new_account {
        *account = patch
            .try_to_new_account()
            .map_err(|error| format!("failed to reconstruct account from patch: {error}"))?;
        Ok(())
    } else {
        account
            .apply_patch(&patch)
            .map_err(|error| format!("failed to apply account patch: {error}"))
    }
}

/// Converts an account-creating delta into an account after merging additional storage updates.
///
/// The caller must know that `delta` comes from the transaction that created the account: a delta
/// that upgrades the code of an existing account also carries code, but does not describe a whole
/// account.
pub fn account_from_full_delta_with_storage_patch(
    delta: &AccountDelta,
    additional_storage: AccountStoragePatch,
) -> Result<Account, String> {
    if delta.code().is_empty() {
        return Err("cannot construct an account from a delta without account code".to_string());
    }

    let patch =
        account_patch_from_parts(AssetVault::default(), Felt::ZERO, delta, additional_storage)?;
    patch
        .try_to_new_account()
        .map_err(|error| format!("failed to construct account from full-state patch: {error}"))
}

fn rebuilds_new_account(account: &Account, delta: &AccountDelta) -> bool {
    account.is_new() && delta.nonce_delta() == Felt::ONE
}

fn account_patch_from_delta(
    account: &Account,
    delta: &AccountDelta,
    additional_storage: AccountStoragePatch,
) -> Result<AccountPatch, String> {
    if account.id() != delta.id() {
        return Err(format!(
            "account delta ID mismatch: expected {}, got {}",
            account.id().to_hex(),
            delta.id().to_hex()
        ));
    }

    if rebuilds_new_account(account, delta) {
        let mut storage_patch =
            AccountStoragePatch::from_entries(account.storage().slots().iter().map(|slot| {
                (
                    slot.name().clone(),
                    StorageSlotPatch::from(slot.content().clone()),
                )
            }))
            .map_err(|error| format!("failed to build full account storage patch: {error}"))?;
        storage_patch
            .merge(delta.storage().clone())
            .and_then(|_| storage_patch.merge(additional_storage))
            .map_err(|error| format!("failed to merge account storage patches: {error}"))?;

        let mut vault_patch = AccountVaultPatch::default();
        for asset in account.vault().assets() {
            vault_patch.insert_asset(asset);
        }
        vault_patch.merge(vault_patch_from_delta(account.vault().clone(), delta)?);

        return AccountPatch::new(
            delta.id(),
            storage_patch,
            vault_patch,
            AccountCodePatch::new(Some(account.code().clone())),
            Some(Felt::ONE),
        )
        .map_err(|error| format!("failed to build full account patch: {error}"));
    }

    account_patch_from_parts(
        account.vault().clone(),
        account.nonce(),
        delta,
        additional_storage,
    )
}

fn account_patch_from_parts(
    final_vault: AssetVault,
    initial_nonce: Felt,
    delta: &AccountDelta,
    additional_storage: AccountStoragePatch,
) -> Result<AccountPatch, String> {
    let vault_patch = vault_patch_from_delta(final_vault, delta)?;

    let mut storage_patch = delta.storage().clone();
    storage_patch
        .merge(additional_storage)
        .map_err(|error| format!("failed to merge account storage patches: {error}"))?;

    let final_nonce = if delta.is_empty() && storage_patch.is_empty() {
        None
    } else {
        Some(initial_nonce + delta.nonce_delta())
    };

    AccountPatch::new(
        delta.id(),
        storage_patch,
        vault_patch,
        delta.code().clone(),
        final_nonce,
    )
    .map_err(|error| format!("failed to build account patch: {error}"))
}

fn vault_patch_from_delta(
    mut final_vault: AssetVault,
    delta: &AccountDelta,
) -> Result<AccountVaultPatch, String> {
    let mut vault_patch = AccountVaultPatch::default();

    for asset in delta.vault().added_assets() {
        let asset_id = asset.id();
        let final_asset = final_vault
            .add_asset(asset)
            .map_err(|error| format!("failed to add asset from delta: {error}"))?;
        vault_patch.insert_asset(final_asset);
        debug_assert_eq!(final_vault.get(asset_id), Some(final_asset));
    }
    for asset in delta.vault().removed_assets() {
        let asset_id = asset.id();
        match final_vault
            .remove_asset(asset)
            .map_err(|error| format!("failed to remove asset from delta: {error}"))?
        {
            Some(final_asset) => vault_patch.insert_asset(final_asset),
            None => vault_patch.remove_asset(asset_id),
        }
    }

    Ok(vault_patch)
}

#[cfg(test)]
mod tests {
    use miden_protocol::account::delta::{AssetDelta, AssetDeltaOperation};
    use miden_protocol::account::{
        Account, AccountCode, AccountCodePatch, AccountDelta, AccountId, AccountIdVersion,
        AccountStorage, AccountStoragePatch, AccountType, AccountVaultDelta, AssetCallbackFlag,
        StorageMapKey, StorageMapPatch, StorageMapPatchEntries, StorageSlotName, StorageSlotPatch,
    };
    use miden_protocol::asset::{AssetVault, FungibleAsset};
    use miden_protocol::testing::storage::{MOCK_MAP_SLOT, MOCK_VALUE_SLOT0, MOCK_VALUE_SLOT1};
    use miden_protocol::{Felt, Word};

    use miden_protocol::testing::add_component::AddComponent;
    use miden_protocol::testing::noop_auth_component::NoopAuthComponent;

    use super::{apply_account_delta, apply_account_delta_with_storage_patch};

    #[test]
    fn applies_create_update_and_remove_storage_operations() {
        let account_id = AccountId::dummy(
            [7_u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let mut account = Account::new_existing(
            account_id,
            AssetVault::default(),
            AccountStorage::mock(),
            AccountCode::mock(),
            Felt::ONE,
        );
        let created_slot = StorageSlotName::new("guardian::test::created").unwrap();
        let updated_value = Word::from([11_u32, 12, 13, 14]);
        let created_value = Word::from([21_u32, 22, 23, 24]);
        let storage = AccountStoragePatch::builder()
            .remove_value(MOCK_VALUE_SLOT0.clone())
            .update_value(MOCK_VALUE_SLOT1.clone(), updated_value)
            .create_value(created_slot.clone(), created_value)
            .build();
        let delta = AccountDelta::new(
            account_id,
            storage,
            AccountVaultDelta::default(),
            AccountCodePatch::default(),
            Felt::ONE,
        )
        .unwrap();

        apply_account_delta(&mut account, &delta).unwrap();

        assert!(account.storage().get(&MOCK_VALUE_SLOT0).is_none());
        assert_eq!(
            account.storage().get(&MOCK_VALUE_SLOT1).unwrap().value(),
            updated_value
        );
        assert_eq!(
            account.storage().get(&created_slot).unwrap().value(),
            created_value
        );
        assert_eq!(account.nonce(), Felt::from(2_u8));
    }

    #[test]
    fn converts_relative_vault_delta_to_absolute_patch() {
        let account_id = AccountId::dummy(
            [8_u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let initial_asset = FungibleAsset::mock(100);
        let mut account = Account::new_existing(
            account_id,
            AssetVault::new(&[initial_asset]).unwrap(),
            AccountStorage::mock(),
            AccountCode::mock(),
            Felt::ONE,
        );
        let removed_asset = FungibleAsset::mock(40);
        let asset_id = initial_asset.id();
        let vault_delta =
            AccountVaultDelta::new([AssetDelta::new(AssetDeltaOperation::Remove, removed_asset)])
                .unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::new(),
            vault_delta,
            AccountCodePatch::default(),
            Felt::ONE,
        )
        .unwrap();

        apply_account_delta(&mut account, &delta).unwrap();

        assert_eq!(account.vault().get(asset_id), Some(FungibleAsset::mock(60)));
    }

    /// A new wallet's first transaction: the account is still at nonce 0, so the delta cannot be
    /// applied as a patch against existing state and the account is rebuilt from a full-state
    /// patch instead. Slots the delta never mentions must survive that rebuild, and the
    /// additional storage patch (the server's replay-protection entry) must merge on top.
    #[test]
    fn rebuilds_new_account_from_first_delta_preserving_untouched_state() {
        let account_id = AccountId::dummy(
            [9_u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let initial_asset = FungibleAsset::mock(100);
        let asset_id = initial_asset.id();
        let mut account = Account::new_unchecked(
            account_id,
            AssetVault::new(&[initial_asset]).unwrap(),
            AccountStorage::mock(),
            AccountCode::mock(),
            Felt::ZERO,
            None,
        );
        assert_eq!(account.nonce(), Felt::ZERO);

        let untouched_value = account.storage().get(&MOCK_VALUE_SLOT0).unwrap().value();
        let untouched_map_root = account.storage().get(&MOCK_MAP_SLOT).unwrap().value();

        let created_slot = StorageSlotName::new("guardian::test::first_tx").unwrap();
        let created_value = Word::from([31_u32, 32, 33, 34]);
        let updated_value = Word::from([41_u32, 42, 43, 44]);
        let vault_delta = AccountVaultDelta::new([AssetDelta::new(
            AssetDeltaOperation::Remove,
            FungibleAsset::mock(40),
        )])
        .unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::builder()
                .update_value(MOCK_VALUE_SLOT1.clone(), updated_value)
                .create_value(created_slot.clone(), created_value)
                .build(),
            vault_delta,
            AccountCodePatch::default(),
            Felt::ONE,
        )
        .unwrap();

        let replay_key = StorageMapKey::new(Word::from([7_u32, 0, 0, 0]));
        let replay_flag = Word::from([1_u32, 0, 0, 0]);
        let additional_storage = AccountStoragePatch::from_entries([(
            MOCK_MAP_SLOT.clone(),
            StorageSlotPatch::Map(StorageMapPatch::Update {
                entries: StorageMapPatchEntries::from_iter([(replay_key, replay_flag)]),
            }),
        )])
        .unwrap();

        apply_account_delta_with_storage_patch(&mut account, &delta, additional_storage).unwrap();

        assert_eq!(account.nonce(), Felt::ONE);
        assert_eq!(
            account.storage().get(&MOCK_VALUE_SLOT0).unwrap().value(),
            untouched_value,
            "a slot the delta never mentions must survive the nonce-0 rebuild"
        );
        assert_eq!(
            account.storage().get(&MOCK_VALUE_SLOT1).unwrap().value(),
            updated_value
        );
        assert_eq!(
            account.storage().get(&created_slot).unwrap().value(),
            created_value
        );
        assert_eq!(
            account
                .storage()
                .get_map_item(&MOCK_MAP_SLOT, replay_key)
                .unwrap(),
            replay_flag,
            "the additional storage patch must merge into the rebuilt account"
        );
        assert_ne!(
            account.storage().get(&MOCK_MAP_SLOT).unwrap().value(),
            untouched_map_root,
            "the replay entry must change the map root it was merged into"
        );
        assert_eq!(account.vault().get(asset_id), Some(FungibleAsset::mock(60)));
    }

    /// Since protocol 0.17.0-rc.8 a code upgrade carries the new code in the delta, so a
    /// code-carrying delta on an account that already executed a transaction replaces its code
    /// instead of being rejected as an account creation.
    #[test]
    fn applies_code_upgrade_to_existing_account() {
        let account_id = AccountId::dummy(
            [10_u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let mut account = Account::new_existing(
            account_id,
            AssetVault::default(),
            AccountStorage::mock(),
            AccountCode::mock(),
            Felt::ONE,
        );
        let upgraded_code =
            AccountCode::from_components(&[NoopAuthComponent.into(), AddComponent.into()]).unwrap();
        assert_ne!(account.code(), &upgraded_code);
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::new(),
            AccountVaultDelta::default(),
            AccountCodePatch::new(Some(upgraded_code.clone())),
            Felt::ONE,
        )
        .unwrap();

        apply_account_delta(&mut account, &delta).unwrap();

        assert_eq!(account.code(), &upgraded_code);
        assert_eq!(account.nonce(), Felt::from(2_u8));
    }

    #[test]
    fn rejects_account_creating_delta_on_new_account() {
        let account_id = AccountId::dummy(
            [11_u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            AssetCallbackFlag::Disabled,
        );
        let mut account = Account::new_unchecked(
            account_id,
            AssetVault::default(),
            AccountStorage::mock(),
            AccountCode::mock(),
            Felt::ZERO,
            None,
        );
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::new(),
            AccountVaultDelta::default(),
            AccountCodePatch::new(Some(account.code().clone())),
            Felt::ONE,
        )
        .unwrap();

        let error = apply_account_delta(&mut account, &delta).unwrap_err();

        assert!(error.contains("account-creating delta"));
    }
}
