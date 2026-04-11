//! Multisig account wrapper with storage inspection helpers.

use miden_client::Serializable;
use miden_protocol::Word;
use miden_protocol::account::{
    Account, AccountId, AccountStorage, StorageMap, StorageMapKey, StorageSlot, StorageSlotName,
};

use crate::error::{MultisigError, Result};
use crate::procedures::ProcedureName;
use crate::proposal::TransactionType;

// Storage slot names for OpenZeppelin multisig/guardian components
const OZ_MULTISIG_THRESHOLD_CONFIG: &str = "openzeppelin::multisig::threshold_config";
const OZ_MULTISIG_SIGNER_PUBKEYS: &str = "openzeppelin::multisig::signer_public_keys";
const OZ_MULTISIG_PROCEDURE_THRESHOLDS: &str = "openzeppelin::multisig::procedure_thresholds";
const OZ_GUARDIAN_SELECTOR: &str = "openzeppelin::guardian::selector";
const OZ_GUARDIAN_PUBLIC_KEY: &str = "openzeppelin::guardian::public_key";

/// Wrapper around a Miden Account with multisig-specific helpers.
///
/// This provides convenient access to multisig configuration stored in account storage:
/// - Threshold config slot: `[threshold, num_signers, 0, 0]`
/// - Signer commitments map slot: `[index, 0, 0, 0] => COMMITMENT`
/// - Executed transactions map slot (replay protection)
/// - Procedure threshold overrides map slot: `PROC_ROOT => [threshold, 0, 0, 0]`
/// - GUARDIAN selector slot: `[1, 0, 0, 0]` (ON) or `[0, 0, 0, 0]` (OFF)
/// - GUARDIAN public key map slot
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
        self.account.nonce().as_canonical_u64()
    }

    /// Returns the account commitment (hash).
    pub fn commitment(&self) -> Word {
        self.account.to_commitment()
    }

    /// Returns a reference to the underlying Account.
    pub fn inner(&self) -> &Account {
        &self.account
    }

    /// Consumes self and returns the underlying Account.
    pub fn into_inner(self) -> Account {
        self.account
    }

    fn get_item_by_name(&self, slot_name: &str) -> Option<Word> {
        let slot_name = StorageSlotName::new(slot_name).ok()?;
        self.account.storage().get_item(&slot_name).ok()
    }

    fn get_map_item_by_name(&self, slot_name: &str, key: Word) -> Option<Word> {
        let slot_name = StorageSlotName::new(slot_name).ok()?;
        self.account.storage().get_map_item(&slot_name, key).ok()
    }

    /// Returns the multisig threshold from storage.
    pub fn threshold(&self) -> Result<u32> {
        let slot_value = self
            .get_item_by_name(OZ_MULTISIG_THRESHOLD_CONFIG)
            .ok_or_else(|| {
                MultisigError::AccountStorage("threshold config slot not found".to_string())
            })?;

        Ok(slot_value[0].as_canonical_u64() as u32)
    }

    /// Returns the number of signers from storage.
    pub fn num_signers(&self) -> Result<u32> {
        let slot_value = self
            .get_item_by_name(OZ_MULTISIG_THRESHOLD_CONFIG)
            .ok_or_else(|| {
                MultisigError::AccountStorage("threshold config slot not found".to_string())
            })?;

        Ok(slot_value[1].as_canonical_u64() as u32)
    }

    /// Returns the configured threshold override for a specific procedure, if present.
    pub fn procedure_threshold(&self, procedure: ProcedureName) -> Result<Option<u32>> {
        let value = self.get_map_item_by_name(OZ_MULTISIG_PROCEDURE_THRESHOLDS, procedure.root());
        let Some(value) = value else {
            return Ok(None);
        };

        if value == Word::default() {
            return Ok(None);
        }

        let threshold = value[0].as_canonical_u64() as u32;
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
            TransactionType::UpdateProcedureThreshold { .. } => {
                ProcedureName::UpdateProcedureThreshold
            }
            TransactionType::SwitchGuardian { .. } => ProcedureName::UpdateGuardian,
        };

        self.effective_threshold_for_procedure(procedure)
    }

    /// Extracts cosigner commitments from signer public keys map slot.
    ///
    /// Returns a vector of commitment Words. Returns empty vector if
    /// the slot is empty or has no entries.
    pub fn cosigner_commitments(&self) -> Vec<Word> {
        self.extract_indexed_map_words(OZ_MULTISIG_SIGNER_PUBKEYS)
    }

    fn extract_indexed_map_words(&self, slot_name: &str) -> Vec<Word> {
        let mut commitments = Vec::new();
        let Ok(slot_name) = StorageSlotName::new(slot_name) else {
            return commitments;
        };

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self.account.storage().get_map_item(&slot_name, key) {
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

    /// Returns whether GUARDIAN verification is enabled.
    pub fn guardian_enabled(&self) -> Result<bool> {
        let slot_value = self.get_item_by_name(OZ_GUARDIAN_SELECTOR).ok_or_else(|| {
            MultisigError::AccountStorage("GUARDIAN selector slot not found".to_string())
        })?;

        Ok(slot_value[0].as_canonical_u64() == 1)
    }

    /// Returns the GUARDIAN server commitment from GUARDIAN public key map slot.
    pub fn guardian_commitment(&self) -> Result<Word> {
        let key = Word::from([0u32, 0, 0, 0]);
        self.get_map_item_by_name(OZ_GUARDIAN_PUBLIC_KEY, key)
            .ok_or_else(|| {
                MultisigError::AccountStorage("GUARDIAN public key slot not found".to_string())
            })
    }

    pub fn with_procedure_threshold(
        &self,
        procedure: ProcedureName,
        threshold: u32,
    ) -> Result<Self> {
        let mut overrides = self.procedure_threshold_overrides()?;
        overrides.retain(|(current, _)| *current != procedure);
        if threshold > 0 {
            overrides.push((procedure, threshold));
        }

        let slot_name = StorageSlotName::new(OZ_MULTISIG_PROCEDURE_THRESHOLDS).map_err(|e| {
            MultisigError::AccountStorage(format!("invalid procedure threshold slot name: {}", e))
        })?;
        let entries = overrides.into_iter().map(|(procedure, threshold)| {
            (
                StorageMapKey::new(procedure.root()),
                Word::from([threshold, 0, 0, 0]),
            )
        });
        let map = StorageMap::with_entries(entries).map_err(|e| {
            MultisigError::AccountStorage(format!("failed to build procedure threshold map: {}", e))
        })?;
        let slot = StorageSlot::with_map(slot_name, map);

        let (id, vault, storage, code, nonce, seed) = self.account.clone().into_parts();
        let storage_slots = storage
            .into_slots()
            .into_iter()
            .filter(|current| current.name().as_str() != OZ_MULTISIG_PROCEDURE_THRESHOLDS)
            .chain([slot])
            .collect();
        let storage = AccountStorage::new(storage_slots).map_err(|e| {
            MultisigError::AccountStorage(format!("failed to rebuild account storage: {}", e))
        })?;
        let account = Account::new_unchecked(id, vault, storage, code, nonce, seed);

        Ok(Self::new(account))
    }
}

#[cfg(test)]
mod tests {
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };
    use miden_protocol::account::{AccountStorage, StorageMap, StorageSlot, StorageSlotName};
    use miden_protocol::note::NoteId;

    use super::*;

    fn word(v: u32) -> Word {
        Word::from([v, 0, 0, 0])
    }

    fn build_test_account() -> MultisigAccount {
        let config = MultisigGuardianConfig::new(2, vec![word(1), word(2), word(3)], word(99))
            .with_proc_threshold_overrides(vec![
                (ProcedureName::SendAsset.root(), 1),
                (ProcedureName::UpdateSigners.root(), 3),
                (ProcedureName::UpdateGuardian.root(), 1),
            ]);

        let account = MultisigGuardianBuilder::new(config)
            .with_seed([7u8; 32])
            .build()
            .expect("account builds");

        MultisigAccount::new(account)
    }

    fn build_account_with_signer_slots(oz_commitments: Vec<Word>) -> MultisigAccount {
        fn signer_slot(slot_name: &str, commitments: Vec<Word>) -> StorageSlot {
            let slot_name = StorageSlotName::new(slot_name).expect("valid slot name");
            let entries = commitments
                .into_iter()
                .enumerate()
                .map(|(index, commitment)| (StorageMapKey::from_index(index as u32), commitment));
            let map = StorageMap::with_entries(entries).expect("valid signer map");
            StorageSlot::with_map(slot_name, map)
        }

        let account =
            MultisigGuardianBuilder::new(MultisigGuardianConfig::new(1, vec![word(1)], word(99)))
                .with_seed([9u8; 32])
                .build_existing()
                .expect("account builds");
        let (id, vault, storage, code, nonce, seed) = account.into_parts();
        let storage_slots = storage
            .into_slots()
            .into_iter()
            .filter(|slot| slot.name().as_str() != OZ_MULTISIG_SIGNER_PUBKEYS)
            .chain([signer_slot(OZ_MULTISIG_SIGNER_PUBKEYS, oz_commitments)])
            .collect();
        let storage = AccountStorage::new(storage_slots).expect("valid storage");
        let account = Account::new_unchecked(id, vault, storage, code, nonce, seed);

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
                .effective_threshold_for_transaction(&TransactionType::SwitchGuardian {
                    new_endpoint: "http://new-guardian.example.com".to_string(),
                    new_commitment: word(11),
                })
                .expect("threshold"),
            1
        );
    }

    #[test]
    fn cosigner_commitments_reads_openzeppelin_signer_map() {
        let account = build_account_with_signer_slots(vec![word(11), word(12)]);

        assert_eq!(account.cosigner_commitments(), vec![word(11), word(12)]);
    }

    #[test]
    fn cosigner_commitments_returns_empty_when_openzeppelin_signer_map_is_empty() {
        let account = build_account_with_signer_slots(Vec::new());

        assert!(account.cosigner_commitments().is_empty());
    }

    #[test]
    fn with_procedure_threshold_updates_existing_override() {
        let account = build_test_account();

        let updated = account
            .with_procedure_threshold(ProcedureName::SendAsset, 2)
            .expect("threshold updated");

        assert_eq!(
            updated
                .procedure_threshold(ProcedureName::SendAsset)
                .expect("threshold lookup"),
            Some(2)
        );
    }

    #[test]
    fn with_procedure_threshold_clears_override_when_zero() {
        let account = build_test_account();

        let updated = account
            .with_procedure_threshold(ProcedureName::SendAsset, 0)
            .expect("threshold cleared");

        assert_eq!(
            updated
                .procedure_threshold(ProcedureName::SendAsset)
                .expect("threshold lookup"),
            None
        );
    }
}
