//! Multisig account wrapper with storage inspection helpers.

use miden_client::Serializable;
use miden_protocol::Word;
use miden_protocol::account::{Account, AccountId, StorageSlotContent, StorageSlotName};

use crate::error::{MultisigError, Result};
use crate::procedures::ProcedureName;
use crate::proposal::TransactionType;

// Storage slot names for OpenZeppelin multisig/psm components
const OZ_MULTISIG_THRESHOLD_CONFIG: &str = "openzeppelin::multisig::threshold_config";
const OZ_MULTISIG_SIGNER_PUBKEYS: &str = "openzeppelin::multisig::signer_public_keys";
const OZ_MULTISIG_PROCEDURE_THRESHOLDS: &str = "openzeppelin::multisig::procedure_thresholds";
const OZ_PSM_SELECTOR: &str = "openzeppelin::psm::selector";
const OZ_PSM_PUBLIC_KEY: &str = "openzeppelin::psm::public_key";

// Alternative slot names for miden-standards auth components
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
}

impl MultisigAccount {
    /// Creates a new MultisigAccount wrapper.
    pub fn new(account: Account) -> Self {
        Self { account }
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

    /// Returns the configured threshold override for a specific procedure, if present.
    pub fn procedure_threshold(&self, procedure: ProcedureName) -> Result<Option<u32>> {
        let value =
            self.get_map_item_by_names(&[OZ_MULTISIG_PROCEDURE_THRESHOLDS], procedure.root());
        let Some(value) = value else {
            return Ok(None);
        };

        if value == Word::default() {
            return Ok(None);
        }

        let threshold = value[0].as_int() as u32;
        if threshold == 0 {
            return Ok(None);
        }

        Ok(Some(threshold))
    }

    /// Returns all configured per-procedure threshold overrides.
    pub fn procedure_threshold_overrides(&self) -> Result<Vec<(ProcedureName, u32)>> {
        let mut overrides = Vec::new();
        for procedure in ProcedureName::all() {
            if let Some(threshold) = self.procedure_threshold(*procedure)? {
                overrides.push((*procedure, threshold));
            }
        }
        Ok(overrides)
    }

    /// Returns the effective threshold for a procedure (override if present, else default).
    pub fn effective_threshold_for_procedure(&self, procedure: ProcedureName) -> Result<u32> {
        Ok(self
            .procedure_threshold(procedure)?
            .unwrap_or(self.threshold()?))
    }

    /// Returns the effective threshold for a transaction type.
    pub fn effective_threshold_for_transaction(&self, tx_type: &TransactionType) -> Result<u32> {
        let procedure = match tx_type {
            TransactionType::P2ID { .. } => ProcedureName::SendAsset,
            TransactionType::ConsumeNotes { .. } => ProcedureName::ReceiveAsset,
            TransactionType::AddCosigner { .. }
            | TransactionType::RemoveCosigner { .. }
            | TransactionType::UpdateSigners { .. } => ProcedureName::UpdateSigners,
            TransactionType::SwitchPsm { .. } => ProcedureName::UpdatePsm,
        };

        self.effective_threshold_for_procedure(procedure)
    }

    /// Extracts cosigner commitments from signer public keys map slot.
    ///
    /// Returns a vector of commitment Words. Returns empty vector if
    /// the slot is empty or has no entries.
    pub fn cosigner_commitments(&self) -> Vec<Word> {
        let mut commitments = Vec::new();

        // Find the map slot name
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

#[cfg(test)]
mod tests {
    use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
    use miden_protocol::note::NoteId;

    use super::*;

    fn word(v: u32) -> Word {
        Word::from([v, 0, 0, 0])
    }

    fn build_test_account() -> MultisigAccount {
        let config = MultisigPsmConfig::new(2, vec![word(1), word(2), word(3)], word(99))
            .with_proc_threshold_overrides(vec![
                (ProcedureName::SendAsset.root(), 1),
                (ProcedureName::UpdateSigners.root(), 3),
                (ProcedureName::UpdatePsm.root(), 1),
            ]);

        let account = MultisigPsmBuilder::new(config)
            .with_seed([7u8; 32])
            .build()
            .expect("account builds");

        MultisigAccount::new(account)
    }

    #[test]
    fn effective_threshold_for_procedure_uses_override_or_default() {
        let account = build_test_account();

        assert_eq!(
            account
                .effective_threshold_for_procedure(ProcedureName::SendAsset)
                .expect("threshold"),
            1
        );
        assert_eq!(
            account
                .effective_threshold_for_procedure(ProcedureName::ReceiveAsset)
                .expect("threshold"),
            2
        );
    }

    #[test]
    fn effective_threshold_for_transaction_maps_to_expected_procedures() {
        let account = build_test_account();
        let account_id =
            AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").expect("account id");

        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::P2ID {
                    recipient: account_id,
                    faucet_id: account_id,
                    amount: 10,
                })
                .expect("threshold"),
            1
        );
        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::ConsumeNotes {
                    note_ids: vec![NoteId::from_raw(word(5))],
                })
                .expect("threshold"),
            2
        );
        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::AddCosigner {
                    new_commitment: word(10),
                })
                .expect("threshold"),
            3
        );
        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::RemoveCosigner {
                    commitment: word(2),
                })
                .expect("threshold"),
            3
        );
        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::UpdateSigners {
                    new_threshold: 2,
                    signer_commitments: vec![word(1), word(2), word(3)],
                })
                .expect("threshold"),
            3
        );
        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::SwitchPsm {
                    new_endpoint: "http://new-psm.example.com".to_string(),
                    new_commitment: word(11),
                })
                .expect("threshold"),
            1
        );
    }
}
