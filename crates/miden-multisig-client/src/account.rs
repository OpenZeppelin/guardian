//! Multisig account wrapper with storage inspection helpers.

use miden_client::Serializable;
use miden_protocol::Word;
use miden_protocol::account::{Account, AccountId, StorageSlotContent, StorageSlotName};

use crate::error::{MultisigError, Result};

const OZ_MULTISIG_THRESHOLD_CONFIG: &str = "openzeppelin::multisig::threshold_config";
const OZ_MULTISIG_SIGNER_PUBKEYS: &str = "openzeppelin::multisig::signer_public_keys";
const OZ_PSM_SELECTOR: &str = "openzeppelin::psm::selector";
const OZ_PSM_PUBLIC_KEY: &str = "openzeppelin::psm::public_key";

const STD_THRESHOLD_CONFIG: &str =
    "miden::standards::auth::falcon512_rpo_multisig::threshold_config";
const STD_APPROVER_PUBKEYS: &str =
    "miden::standards::auth::falcon512_rpo_multisig::approver_public_keys";

/// Wrapper around a Miden Account with multisig-specific helpers.
///
/// This provides convenient access to multisig configuration stored in account storage:
/// - Threshold config slot: `[threshold, num_signers, 0, 0]`
/// - Signer commitments map slot: `[index, 0, 0, 0] => COMMITMENT`
/// - Executed transactions map slot (replay protection)
/// - Procedure threshold overrides map slot: `PROC_ROOT => [threshold, 0, 0, 0]`
/// - PSM selector slot: `[1, 0, 0, 0]` (ON) or `[0, 0, 0, 0]` (OFF)
/// - PSM public key map slot
#[derive(Debug, Clone)]
pub struct MultisigAccount {
    account: Account,
    psm_endpoint: String,
}

impl MultisigAccount {
    /// Creates a new MultisigAccount wrapper.
    pub fn new(account: Account, psm_endpoint: impl Into<String>) -> Self {
        Self {
            account,
            psm_endpoint: psm_endpoint.into(),
        }
    }

    /// Returns the account ID.
    pub fn id(&self) -> AccountId {
        self.account.id()
    }

    /// Returns the account nonce.
    pub fn nonce(&self) -> u64 {
        self.account.nonce().as_int()
    }

    /// Returns the account commitment (hash).
    pub fn commitment(&self) -> Word {
        self.account.commitment()
    }

    /// Returns the associated PSM endpoint.
    pub fn psm_endpoint(&self) -> &str {
        &self.psm_endpoint
    }

    /// Returns a reference to the underlying Account.
    pub fn inner(&self) -> &Account {
        &self.account
    }

    /// Consumes self and returns the underlying Account.
    pub fn into_inner(self) -> Account {
        self.account
    }

    /// Helper to get a storage item by trying multiple slot names
    fn get_item_by_names(&self, names: &[&str]) -> Option<Word> {
        for name in names {
            if let Ok(slot_name) = StorageSlotName::new(*name)
                && let Ok(value) = self.account.storage().get_item(&slot_name)
            {
                return Some(value);
            }
        }
        None
    }

    /// Helper to get a map item by trying multiple slot names
    fn get_map_item_by_names(&self, names: &[&str], key: Word) -> Option<Word> {
        for name in names {
            if let Ok(slot_name) = StorageSlotName::new(*name)
                && let Ok(value) = self.account.storage().get_map_item(&slot_name, key)
            {
                return Some(value);
            }
        }
        None
    }

    /// Find a map slot by checking multiple possible names
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

    /// Returns the multisig threshold from storage.
    pub fn threshold(&self) -> Result<u32> {
        let slot_value = self
            .get_item_by_names(&[OZ_MULTISIG_THRESHOLD_CONFIG, STD_THRESHOLD_CONFIG])
            .ok_or_else(|| {
                MultisigError::AccountStorage("threshold config slot not found".to_string())
            })?;

        Ok(slot_value[0].as_int() as u32)
    }

    /// Returns the number of signers from storage.
    pub fn num_signers(&self) -> Result<u32> {
        let slot_value = self
            .get_item_by_names(&[OZ_MULTISIG_THRESHOLD_CONFIG, STD_THRESHOLD_CONFIG])
            .ok_or_else(|| {
                MultisigError::AccountStorage("threshold config slot not found".to_string())
            })?;

        Ok(slot_value[1].as_int() as u32)
    }

    /// Extracts cosigner commitments from signer public keys map slot.
    ///
    /// Returns a vector of commitment Words. Returns empty vector if
    /// the slot is empty or has no entries.
    pub fn cosigner_commitments(&self) -> Vec<Word> {
        let mut commitments = Vec::new();

        let Some(slot_name) =
            self.find_map_slot_name(&[OZ_MULTISIG_SIGNER_PUBKEYS, STD_APPROVER_PUBKEYS])
        else {
            return commitments;
        };

        let key_zero = Word::from([0u32, 0, 0, 0]);
        let slot_name_ref = StorageSlotName::new(slot_name.clone()).ok();
        let Some(slot_name_ref) = slot_name_ref else {
            return commitments;
        };

        let first_entry = self
            .account
            .storage()
            .get_map_item(&slot_name_ref, key_zero);

        if first_entry.is_err() || first_entry.as_ref().unwrap() == &Word::default() {
            return commitments;
        }

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self.account.storage().get_map_item(&slot_name_ref, key) {
                Ok(value) if value != Word::default() => {
                    commitments.push(value);
                    index += 1;
                }
                _ => break,
            }
        }

        commitments
    }

    /// Extracts cosigner commitments as hex strings with 0x prefix.
    pub fn cosigner_commitments_hex(&self) -> Vec<String> {
        self.cosigner_commitments()
            .into_iter()
            .map(|word| format!("0x{}", hex::encode(word.to_bytes())))
            .collect()
    }

    /// Checks if the given commitment is a cosigner of this account.
    pub fn is_cosigner(&self, commitment: &Word) -> bool {
        self.cosigner_commitments().contains(commitment)
    }

    /// Returns whether PSM verification is enabled.
    pub fn psm_enabled(&self) -> Result<bool> {
        let slot_value = self.get_item_by_names(&[OZ_PSM_SELECTOR]).ok_or_else(|| {
            MultisigError::AccountStorage("PSM selector slot not found".to_string())
        })?;

        Ok(slot_value[0].as_int() == 1)
    }

    /// Returns the PSM server commitment from PSM public key map slot.
    pub fn psm_commitment(&self) -> Result<Word> {
        let key = Word::from([0u32, 0, 0, 0]);
        self.get_map_item_by_names(&[OZ_PSM_PUBLIC_KEY], key)
            .ok_or_else(|| {
                MultisigError::AccountStorage("PSM public key slot not found".to_string())
            })
    }
}
