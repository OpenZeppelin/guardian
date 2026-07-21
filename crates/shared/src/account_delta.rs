use miden_protocol::Felt;
use miden_protocol::account::{
    Account, AccountDelta, AccountPatch, AccountStoragePatch, AccountVaultPatch, StorageSlotPatch,
};
use miden_protocol::asset::AssetVault;

/// Applies a partial [`AccountDelta`] to an existing account.
pub fn apply_account_delta(account: &mut Account, delta: &AccountDelta) -> Result<(), String> {
    apply_account_delta_with_storage_patch(account, delta, AccountStoragePatch::new())
}

/// Applies a partial [`AccountDelta`] and additional storage updates atomically.
///
/// The additional patch is merged into the delta's storage patch before application. This is
/// useful when transaction simulation omits deterministic storage writes that occur during
/// authentication.
pub fn apply_account_delta_with_storage_patch(
    account: &mut Account,
    delta: &AccountDelta,
    additional_storage: AccountStoragePatch,
) -> Result<(), String> {
    if delta.is_full_state() {
        return Err("cannot apply a full-state account delta to an existing account".to_string());
    }

    let patch = account_patch_from_delta(account, delta, additional_storage)?;
    if patch.is_full_state() {
        *account = Account::try_from(&patch)
            .map_err(|error| format!("failed to reconstruct account from patch: {error}"))?;
        Ok(())
    } else {
        account
            .apply_patch(&patch)
            .map_err(|error| format!("failed to apply account patch: {error}"))
    }
}

/// Converts a full-state delta into an account after merging additional storage updates.
pub fn account_from_full_delta_with_storage_patch(
    delta: &AccountDelta,
    additional_storage: AccountStoragePatch,
) -> Result<Account, String> {
    if !delta.is_full_state() {
        return Err("cannot construct an account from a partial account delta".to_string());
    }

    let patch =
        account_patch_from_parts(AssetVault::default(), Felt::ZERO, delta, additional_storage)?;
    Account::try_from(&patch)
        .map_err(|error| format!("failed to construct account from full-state patch: {error}"))
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

    if account.nonce() == Felt::ZERO && delta.nonce_delta() == Felt::ONE {
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
            Some(account.code().clone()),
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
        delta.code().cloned(),
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
    use miden_protocol::account::{
        Account, AccountCode, AccountDelta, AccountId, AccountIdVersion, AccountStorage,
        AccountStoragePatch, AccountType, AccountVaultDelta, AssetCallbackFlag, StorageSlotName,
    };
    use miden_protocol::asset::{AssetVault, FungibleAsset};
    use miden_protocol::testing::storage::{MOCK_VALUE_SLOT0, MOCK_VALUE_SLOT1};
    use miden_protocol::{Felt, Word};

    use super::apply_account_delta;

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
            None,
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
        let mut vault_delta = AccountVaultDelta::default();
        vault_delta.remove_asset(removed_asset).unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStoragePatch::new(),
            vault_delta,
            None,
            Felt::ONE,
        )
        .unwrap();

        apply_account_delta(&mut account, &delta).unwrap();

        assert_eq!(account.vault().get(asset_id), Some(FungibleAsset::mock(60)));
    }
}
