use miden_protocol::Word;
use miden_protocol::account::{Account, StorageSlotName};
use miden_protocol::utils::serde::Serializable;
use miden_standards::account::auth::AuthGuardedMultisig;

// `AuthGuardedMultisig` storage slot names (miden::standards::auth::*),
// sourced from the component's `*_slot()` accessors so they cannot drift.
fn multisig_threshold_config_slot() -> &'static str {
    AuthGuardedMultisig::threshold_config_slot().as_str()
}
fn multisig_approver_pubkeys_slot() -> &'static str {
    AuthGuardedMultisig::approver_public_keys_slot().as_str()
}

/// Slot name of the GUARDIAN public key map (`miden::standards::auth::guardian::pub_key`).
pub fn guardian_public_key_slot_name() -> &'static str {
    AuthGuardedMultisig::guardian_public_key_slot().as_str()
}

pub struct MidenAccountInspector<'a> {
    account: &'a Account,
}

impl<'a> MidenAccountInspector<'a> {
    pub fn new(account: &'a Account) -> Self {
        Self { account }
    }

    /// Try to get a value from storage by slot name, returning None if not found or invalid
    fn get_item_by_name(&self, slot_name: &str) -> Option<Word> {
        let name = StorageSlotName::new(slot_name).ok()?;
        self.account.storage().get_item(&name).ok()
    }

    /// Try to get a map item from storage by slot name, returning None if not found or invalid
    fn get_map_item_by_name(&self, slot_name: &str, key: Word) -> Option<Word> {
        let name = StorageSlotName::new(slot_name).ok()?;
        self.account
            .storage()
            .get_map_item(&name, miden_protocol::account::StorageMapKey::new(key))
            .ok()
    }

    /// Extract public keys from the multisig signer map.
    ///
    /// Returns an empty vector if the signer map is empty or missing.
    pub fn extract_pubkeys(&self) -> Vec<String> {
        self.extract_map_pubkeys(multisig_approver_pubkeys_slot())
    }

    /// Extract public keys from slot 1 of the multisig signer map.
    pub fn extract_slot_1_pubkeys(&self) -> Vec<String> {
        self.extract_pubkeys()
    }

    fn extract_map_pubkeys(&self, slot_name: &str) -> Vec<String> {
        let mut pubkeys = Vec::new();

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self.get_map_item_by_name(slot_name, key) {
                Some(value) if value != Word::default() => {
                    let pubkey_hex = format!("0x{}", hex::encode(value.to_bytes()));
                    pubkeys.push(pubkey_hex);
                    index += 1;
                }
                _ => break,
            }
        }

        pubkeys
    }

    /// Check if a public key exists in the multisig signer map.
    pub fn pubkey_exists(&self, target_pubkey: &str) -> bool {
        self.extract_pubkeys().iter().any(|pk| pk == target_pubkey)
    }

    /// Check if the account is an `AuthGuardedMultisig` by the presence of a GUARDIAN
    /// public key. The component has no enable/disable selector — the guardian is always
    /// present — so a non-default key in the guardian pub_key slot uniquely identifies a
    /// guarded multisig.
    #[cfg(test)]
    pub fn has_guardian_auth(&self) -> bool {
        self.extract_guardian_public_key().is_some()
    }

    /// Whether the account carries the multisig auth component, detected by the presence of its
    /// `threshold_config` slot. Preferred over the guardian-key check for gating the
    /// replay-protection adjustment, because the `executed_transactions` map belongs to the
    /// multisig component and the adjustment must run for every multisig tx regardless of the
    /// guardian key's value.
    ///
    /// Note this is NOT delta-invariant: `StorageValuePatch::Remove` removes a value slot
    /// outright, so callers must evaluate it on the pre-delta account (the state that was
    /// authenticated) rather than the post-delta one.
    pub fn has_multisig_auth(&self) -> bool {
        self.get_item_by_name(multisig_threshold_config_slot())
            .is_some()
    }

    /// Extract GUARDIAN public key commitment from the GUARDIAN public key map
    /// (`miden::standards::auth::guardian::pub_key`).
    pub fn extract_guardian_public_key(&self) -> Option<String> {
        let key_zero = Word::from([0u32, 0, 0, 0]);
        let value = self.get_map_item_by_name(guardian_public_key_slot_name(), key_zero)?;

        if value == Word::default() {
            return None;
        }

        Some(format!("0x{}", hex::encode(value.to_bytes())))
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use guardian_shared::FromJson;
    use miden_protocol::account::{
        AccountCode, AccountId, AccountIdVersion, AccountStorage, AccountType, StorageMap,
        StorageMapKey, StorageSlot, StorageSlotName,
    };
    use miden_protocol::asset::AssetVault;

    fn word(v: u32) -> Word {
        Word::from([v, 0, 0, 0])
    }

    fn build_account_with_signer_slots(oz_pubkeys: Vec<Word>) -> Account {
        fn signer_slot(slot_name: &str, pubkeys: Vec<Word>) -> StorageSlot {
            let slot_name = StorageSlotName::new(slot_name).expect("valid slot name");
            let entries = pubkeys.into_iter().enumerate().map(|(index, pubkey)| {
                (
                    StorageMapKey::new(Word::from([index as u32, 0, 0, 0])),
                    pubkey,
                )
            });
            let map = StorageMap::with_entries(entries).expect("valid signer map");
            StorageSlot::with_map(slot_name, map)
        }

        let threshold_slot = StorageSlot::with_value(
            StorageSlotName::new(multisig_threshold_config_slot()).expect("valid slot name"),
            Word::from([1u32, 1, 0, 0]),
        );
        let storage = AccountStorage::new(vec![
            threshold_slot,
            signer_slot(multisig_approver_pubkeys_slot(), oz_pubkeys),
        ])
        .expect("valid storage");
        let account_id = AccountId::dummy(
            [3u8; 15],
            AccountIdVersion::Version1,
            AccountType::Private,
            miden_protocol::account::AssetCallbackFlag::Disabled,
        );

        Account::new_existing(
            account_id,
            AssetVault::new(&[]).expect("empty vault"),
            storage,
            AccountCode::mock(),
            miden_protocol::Felt::new_unchecked(1),
        )
    }

    #[test]
    fn test_pubkey_exists() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        let pubkey = inspector
            .extract_pubkeys()
            .into_iter()
            .next()
            .expect("Expected at least one signer pubkey in the approver map");

        assert!(
            inspector.pubkey_exists(&pubkey),
            "Signer pubkey should exist in storage"
        );

        assert!(
            !inspector.pubkey_exists("0xdeadbeef"),
            "Random pubkey should not exist"
        );
    }

    #[test]
    fn test_has_guardian_auth() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        assert!(
            inspector.has_guardian_auth(),
            "Fixture account should have GUARDIAN auth enabled (auth_tx_falcon512_poseidon2_multisig procedure)"
        );
    }

    #[test]
    fn test_extract_pubkeys_reads_approver_signer_map() {
        let account = build_account_with_signer_slots(vec![word(11), word(12)]);
        let inspector = MidenAccountInspector::new(&account);

        assert_eq!(
            inspector.extract_pubkeys(),
            vec![
                format!("0x{}", hex::encode(word(11).to_bytes())),
                format!("0x{}", hex::encode(word(12).to_bytes())),
            ]
        );
    }

    #[test]
    fn test_extract_guardian_public_key() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        let guardian_pubkey = inspector.extract_guardian_public_key();
        assert!(
            guardian_pubkey.is_some(),
            "Expected GUARDIAN public key from the guardian pub_key slot"
        );
        assert!(
            guardian_pubkey.unwrap().starts_with("0x"),
            "GUARDIAN public key should be hex format"
        );
    }

    #[test]
    fn test_extract_guardian_public_key_empty_value() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let mut account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let slot_name = StorageSlotName::new(guardian_public_key_slot_name())
            .expect("Failed to parse GUARDIAN public key slot");
        let key_zero = Word::from([0u32, 0, 0, 0]);

        account
            .storage_mut()
            .set_map_item(&slot_name, StorageMapKey::new(key_zero), Word::default())
            .expect("Failed to overwrite GUARDIAN public key value");

        let inspector = MidenAccountInspector::new(&account);
        assert!(
            inspector.extract_guardian_public_key().is_none(),
            "Expected None for empty/default GUARDIAN public key value"
        );
    }

    #[test]
    fn test_extract_pubkeys_returns_empty_when_openzeppelin_signer_map_is_empty() {
        let account = build_account_with_signer_slots(Vec::new());
        let inspector = MidenAccountInspector::new(&account);

        assert!(inspector.extract_pubkeys().is_empty());
    }
}
