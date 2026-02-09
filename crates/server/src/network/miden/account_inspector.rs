use miden_protocol::Word;
use miden_protocol::account::{Account, StorageSlotContent, StorageSlotName};
use miden_protocol::utils::Serializable;

// Storage slot names for OpenZeppelin multisig/psm components
const OZ_MULTISIG_THRESHOLD_CONFIG: &str = "openzeppelin::multisig::threshold_config";
const OZ_MULTISIG_SIGNER_PUBKEYS: &str = "openzeppelin::multisig::signer_public_keys";
const OZ_PSM_SELECTOR: &str = "openzeppelin::psm::selector";

// Alternative slot names for miden-standards auth components
const STD_THRESHOLD_CONFIG: &str =
    "miden::standards::auth::falcon512_rpo_multisig::threshold_config";
const STD_APPROVER_PUBKEYS: &str =
    "miden::standards::auth::falcon512_rpo_multisig::approver_public_keys";

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

    /// Find a map slot by checking multiple possible names, returns slot name if found
    fn find_map_slot_name(&self, candidates: &[&str]) -> Option<String> {
        for slot in self.account.storage().slots() {
            let name_str = slot.name().as_str();
            if candidates.contains(&name_str)
                && matches!(slot.content(), StorageSlotContent::Map(_))
            {
                return Some(name_str.to_string());
            }
        }
        None
    }

    /// Find a value slot by checking multiple possible names, returns slot name if found
    fn find_value_slot_name(&self, candidates: &[&str]) -> Option<String> {
        for slot in self.account.storage().slots() {
            let name_str = slot.name().as_str();
            if candidates.contains(&name_str)
                && matches!(slot.content(), StorageSlotContent::Value(_))
            {
                return Some(name_str.to_string());
            }
        }
        None
    }

    /// Extract public key from threshold config slot (single signer case)
    /// Returns None if slot is empty or default
    pub fn extract_slot_0_pubkey(&self) -> Option<String> {
        // Try both OpenZeppelin and miden-standards slot names
        let candidates = [OZ_MULTISIG_THRESHOLD_CONFIG, STD_THRESHOLD_CONFIG];
        let slot_name = self.find_value_slot_name(&candidates)?;
        let value = self.get_item_by_name(&slot_name)?;

        if value != Word::default() {
            let pubkey_hex = format!("0x{}", hex::encode(value.to_bytes()));
            return Some(pubkey_hex);
        }
        None
    }

    /// Extract public keys from signer public keys map slot (multisig mapping)
    /// Returns empty vector if slot is empty or has no entries
    pub fn extract_slot_1_pubkeys(&self) -> Vec<String> {
        let mut pubkeys = Vec::new();

        // Try both OpenZeppelin and miden-standards slot names
        let candidates = [OZ_MULTISIG_SIGNER_PUBKEYS, STD_APPROVER_PUBKEYS];
        let Some(slot_name) = self.find_map_slot_name(&candidates) else {
            return pubkeys;
        };

        let key_zero = Word::from([0u32, 0, 0, 0]);
        let first_entry = self.get_map_item_by_name(&slot_name, key_zero);

        if first_entry.is_none() || first_entry.as_ref().unwrap() == &Word::default() {
            return pubkeys;
        }

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self.get_map_item_by_name(&slot_name, key) {
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

    /// Extract all public keys from account storage
    /// Checks both threshold config slot (single signer) and signer pubkeys map (multisig mapping)
    pub fn extract_all_pubkeys(&self) -> Vec<String> {
        let mut all_pubkeys = Vec::new();

        if let Some(pubkey) = self.extract_slot_0_pubkey() {
            all_pubkeys.push(pubkey);
        }

        let signer_pubkeys = self.extract_slot_1_pubkeys();
        all_pubkeys.extend(signer_pubkeys);

        all_pubkeys
    }

    /// Check if a public key exists in account storage
    /// Returns true if the pubkey is found in either threshold config or signer pubkeys map
    pub fn pubkey_exists(&self, target_pubkey: &str) -> bool {
        if let Some(slot_0_pubkey) = self.extract_slot_0_pubkey()
            && slot_0_pubkey == target_pubkey
        {
            return true;
        }

        let slot_1_pubkeys = self.extract_slot_1_pubkeys();
        slot_1_pubkeys.iter().any(|pk| pk == target_pubkey)
    }

    /// Check if the account has PSM auth enabled by checking the PSM selector storage slot.
    ///
    /// PSM-enabled accounts have the PSM component which stores a selector.
    /// PSM_ON = [1, 0, 0, 0].
    pub fn has_psm_auth(&self) -> bool {
        let Some(selector_value) = self.get_item_by_name(OZ_PSM_SELECTOR) else {
            return false;
        };

        // PSM_ON value indicating PSM is enabled
        let psm_on = Word::from([1u32, 0, 0, 0]);
        selector_value == psm_on
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use private_state_manager_shared::FromJson;

    #[test]
    fn test_extract_slot_0_pubkey() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        let pubkey = inspector.extract_slot_0_pubkey();
        assert!(pubkey.is_some(), "Expected pubkey in threshold config slot");
        assert!(
            pubkey.unwrap().starts_with("0x"),
            "Pubkey should be hex format"
        );
    }

    #[test]
    fn test_extract_all_pubkeys() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        let pubkeys = inspector.extract_all_pubkeys();
        assert!(!pubkeys.is_empty(), "Expected at least one pubkey");

        for pubkey in pubkeys {
            assert!(pubkey.starts_with("0x"), "Pubkey should be hex format");
        }
    }

    #[test]
    fn test_pubkey_exists() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        let pubkey = inspector
            .extract_slot_0_pubkey()
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
    fn test_has_psm_auth() {
        let fixture_json: serde_json::Value =
            serde_json::from_str(crate::testing::fixtures::ACCOUNT_JSON)
                .expect("Failed to parse fixture");

        let account = Account::from_json(&fixture_json).expect("Failed to deserialize account");
        let inspector = MidenAccountInspector::new(&account);

        assert!(
            inspector.has_psm_auth(),
            "Fixture account should have PSM auth enabled (auth_tx_falcon512_rpo_multisig procedure)"
        );
    }
}
