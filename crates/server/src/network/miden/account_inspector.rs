use miden_protocol::Word;
use miden_protocol::account::{Account, StorageSlotName};
use miden_protocol::utils::serde::Serializable;

// Storage slot names for OpenZeppelin multisig/guardian components
const OZ_MULTISIG_THRESHOLD_CONFIG: &str = "openzeppelin::multisig::threshold_config";
const OZ_MULTISIG_SIGNER_PUBKEYS: &str = "openzeppelin::multisig::signer_public_keys";
const OZ_GUARDIAN_SELECTOR: &str = "openzeppelin::guardian::selector";
pub const OZ_GUARDIAN_PUBLIC_KEY: &str = "openzeppelin::guardian::public_key";

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
        self.account.storage().get_map_item(&name, key).ok()
    }

    /// Extract public key from threshold config slot (single signer case)
    /// Returns None if slot is empty or default
    pub fn extract_single_pubkey(&self) -> Option<String> {
        let value = self.get_item_by_name(OZ_MULTISIG_THRESHOLD_CONFIG)?;

        if value != Word::default() {
            let pubkey_hex = format!("0x{}", hex::encode(value.to_bytes()));
            return Some(pubkey_hex);
        }
        None
    }

    /// Extract public keys from the multisig signer map.
    ///
    /// Returns an empty vector if the signer map is empty or missing.
    pub fn extract_pubkeys(&self) -> Vec<String> {
        self.extract_map_pubkeys(OZ_MULTISIG_SIGNER_PUBKEYS)
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

    /// Check if a public key exists in account storage
    /// Returns true if the pubkey is found in either threshold config or signer pubkeys map
    pub fn pubkey_exists(&self, target_pubkey: &str) -> bool {
        if let Some(single_pubkey) = self.extract_single_pubkey()
            && single_pubkey == target_pubkey
        {
            return true;
        }

        let signer_pubkeys = self.extract_pubkeys();
        signer_pubkeys.iter().any(|pk| pk == target_pubkey)
    }

    /// Check if the account has GUARDIAN auth enabled by checking the GUARDIAN selector storage slot.
    ///
    /// GUARDIAN-enabled accounts have the GUARDIAN component which stores a selector.
    /// GUARDIAN_ON = [1, 0, 0, 0].
    pub fn has_guardian_auth(&self) -> bool {
        let Some(selector_value) = self.get_item_by_name(OZ_GUARDIAN_SELECTOR) else {
            return false;
        };

        // GUARDIAN_ON value indicating GUARDIAN is enabled
        let guardian_on = Word::from([1u32, 0, 0, 0]);
        selector_value == guardian_on
    }

    /// Extract GUARDIAN public key commitment from the OpenZeppelin GUARDIAN public key map.
    /// Requires the exact slot name `openzeppelin::guardian::public_key`.
    pub fn extract_guardian_public_key(&self) -> Option<String> {
        let key_zero = Word::from([0u32, 0, 0, 0]);
        let value = self.get_map_item_by_name(OZ_GUARDIAN_PUBLIC_KEY, key_zero)?;

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
        AccountCode, AccountId, AccountIdVersion, AccountStorage, AccountStorageMode, AccountType,
        StorageMap, StorageMapKey, StorageSlot, StorageSlotName,
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
            StorageSlotName::new(OZ_MULTISIG_THRESHOLD_CONFIG).expect("valid slot name"),
            Word::from([1u32, 1, 0, 0]),
        );
        let storage = AccountStorage::new(vec![
            threshold_slot,
            signer_slot(OZ_MULTISIG_SIGNER_PUBKEYS, oz_pubkeys),
        ])
        .expect("valid storage");
        let account_id = AccountId::dummy(
            [3u8; 15],
            AccountIdVersion::Version0,
            AccountType::RegularAccountUpdatableCode,
            AccountStorageMode::Private,
        );

        Account::new_existing(
            account_id,
            AssetVault::new(&[]).expect("empty vault"),
            storage,
            AccountCode::mock(),
            miden_protocol::Felt::new(1),
        )
    }

    #[test]
    fn test_extract_single_pubkey() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        let pubkey = inspector.extract_single_pubkey();
        assert!(pubkey.is_some(), "Expected pubkey in threshold config slot");
        assert!(
            pubkey.unwrap().starts_with("0x"),
            "Pubkey should be hex format"
        );
    }

    #[test]
    fn test_pubkey_exists() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        let pubkey = inspector
            .extract_single_pubkey()
            .expect("Expected pubkey in threshold config slot");

        assert!(
            inspector.pubkey_exists(&pubkey),
            "Pubkey should exist in storage"
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
    fn test_extract_pubkeys_reads_openzeppelin_signer_map() {
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
            "Expected GUARDIAN public key from openzeppelin::guardian::public_key slot"
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
        let slot_name = StorageSlotName::new(OZ_GUARDIAN_PUBLIC_KEY)
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
