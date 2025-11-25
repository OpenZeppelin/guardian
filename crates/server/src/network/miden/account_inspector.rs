use miden_objects::Word;
use miden_objects::account::Account;
use miden_objects::utils::{Deserializable, Serializable};

pub struct MidenAccountInspector<'a> {
    account: &'a Account,
}

impl<'a> MidenAccountInspector<'a> {
    pub fn new(account: &'a Account) -> Self {
        Self { account }
    }

    /// Extract public key from account storage slot 0 (single signer)
    /// Returns None if slot 0 is empty or default
    pub fn extract_slot_0_pubkey(&self) -> Option<String> {
        let slot_0_result = self.account.storage().get_item(0);
        if let Ok(slot_0_value) = slot_0_result
            && slot_0_value != Word::default()
        {
            let pubkey_hex = format!("0x{}", hex::encode(slot_0_value.to_bytes()));
            return Some(pubkey_hex);
        }
        None
    }

    /// Extract public keys from account storage slot 1 (multisig mapping)
    /// Returns empty vector if slot 1 is empty or has no entries
    pub fn extract_slot_1_pubkeys(&self) -> Vec<String> {
        let mut pubkeys = Vec::new();

        let key_zero = Word::from([0u32, 0, 0, 0]);
        let first_entry = self.account.storage().get_map_item(1, key_zero);

        if first_entry.is_err() || first_entry.as_ref().unwrap() == &Word::default() {
            return pubkeys;
        }

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self.account.storage().get_map_item(1, key) {
                Ok(value) if value != Word::default() => {
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
    /// Checks both slot 0 (single signer) and slot 1 (multisig mapping)
    pub fn extract_all_pubkeys(&self) -> Vec<String> {
        let mut all_pubkeys = Vec::new();

        if let Some(pubkey) = self.extract_slot_0_pubkey() {
            all_pubkeys.push(pubkey);
        }

        let slot_1_pubkeys = self.extract_slot_1_pubkeys();
        all_pubkeys.extend(slot_1_pubkeys);

        all_pubkeys
    }

    /// Check if a public key exists in account storage
    /// Returns true if the pubkey is found in either slot 0 or slot 1
    pub fn pubkey_exists(&self, target_pubkey: &str) -> bool {
        if let Some(slot_0_pubkey) = self.extract_slot_0_pubkey()
            && slot_0_pubkey == target_pubkey
        {
            return true;
        }

        let slot_1_pubkeys = self.extract_slot_1_pubkeys();
        slot_1_pubkeys.iter().any(|pk| pk == target_pubkey)
    }

    /// Check if the account code includes the verify_psm_signature procedure
    pub fn has_psm_auth(&self) -> bool {
        const VERIFY_PSM_SIGNATURE_HEX: &str =
            "0506d280235f40b9218b2e2b9cd13adc776dbc139455624f50e3611c5f313506";
        let bytes = hex::decode(VERIFY_PSM_SIGNATURE_HEX)
            .expect("verify_psm_signature root hex must be valid");
        let proc_root =
            Word::read_from_bytes(&bytes).expect("failed to deserialize verify_psm_signature root");

        self.account.code().has_procedure(proc_root)
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
        assert!(pubkey.is_some(), "Expected pubkey in slot 0");
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
            .expect("Expected pubkey in slot 0");

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
            "Fixture account should include verify_psm_signature procedure"
        );
    }
}
