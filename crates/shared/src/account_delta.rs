use miden_protocol::Felt;
use miden_protocol::account::{Account, AccountDelta};

/// Applies a partial [`AccountDelta`] to an existing account.
///
/// Miden 0.16 removed `Account::apply_delta` while transaction summaries still
/// carry relative deltas, so state reconstruction applies the vault delta
/// asset-by-asset and the storage patch through the public storage mutators.
/// Slot-level create/remove operations only occur in full-state deltas, which
/// callers convert via `Account::try_from` instead of applying here.
pub fn apply_account_delta(account: &mut Account, delta: &AccountDelta) -> Result<(), String> {
    if delta.storage().is_empty() && delta.vault().is_empty() && delta.nonce_delta() == Felt::ZERO {
        return Ok(());
    }

    for asset in delta.vault().added_assets() {
        account
            .vault_mut()
            .add_asset(asset)
            .map_err(|e| format!("failed to add asset from delta: {e}"))?;
    }
    for asset in delta.vault().removed_assets() {
        account
            .vault_mut()
            .remove_asset(asset)
            .map_err(|e| format!("failed to remove asset from delta: {e}"))?;
    }

    for (slot_name, value_patch) in delta.storage().values() {
        let value = value_patch.value().ok_or_else(|| {
            format!("unsupported storage slot removal in partial delta for slot {slot_name}")
        })?;
        account
            .storage_mut()
            .set_item(slot_name, value)
            .map_err(|e| format!("failed to apply storage value for slot {slot_name}: {e}"))?;
    }
    for (slot_name, map_patch) in delta.storage().maps() {
        let entries = map_patch.entries().ok_or_else(|| {
            format!("unsupported storage map removal in partial delta for slot {slot_name}")
        })?;
        for (key, value) in entries.as_map() {
            account
                .storage_mut()
                .set_map_item(slot_name, *key, *value)
                .map_err(|e| format!("failed to apply map entry for slot {slot_name}: {e}"))?;
        }
    }

    account
        .increment_nonce(delta.nonce_delta())
        .map_err(|e| format!("failed to increment nonce from delta: {e}"))?;

    Ok(())
}
