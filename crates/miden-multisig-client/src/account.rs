//! Multisig account wrapper with storage inspection helpers.

use miden_client::Serializable;
use miden_protocol::Word;
use miden_protocol::account::{
    Account, AccountId, AccountStorage, StorageMap, StorageMapKey, StorageSlot, StorageSlotName,
};

use miden_standards::account::auth::AuthGuardedMultisig;

use crate::error::{MultisigError, Result};
use crate::procedures::ProcedureName;
use crate::proposal::TransactionType;

/// `AuthGuardedMultisig` storage slot names (`miden::standards::auth::*`), sourced from the
/// component's `*_slot()` accessors so they cannot drift.
fn multisig_threshold_config_slot() -> &'static str {
    AuthGuardedMultisig::threshold_config_slot().as_str()
}
fn multisig_approver_pubkeys_slot() -> &'static str {
    AuthGuardedMultisig::approver_public_keys_slot().as_str()
}
fn multisig_procedure_thresholds_slot() -> &'static str {
    AuthGuardedMultisig::procedure_thresholds_slot().as_str()
}
fn guardian_public_key_slot() -> &'static str {
    AuthGuardedMultisig::guardian_public_key_slot().as_str()
}

/// Wrapper around a Miden Account with multisig-specific helpers.
///
/// This provides convenient access to multisig configuration stored in account storage:
/// - Threshold config slot: `[threshold, num_signers, 0, 0]`
/// - Signer commitments map slot: `[index, 0, 0, 0] => COMMITMENT`
/// - Executed transactions map slot (replay protection)
/// - Procedure threshold overrides map slot: `PROC_ROOT => [threshold, 0, 0, 0]`
/// - GUARDIAN public key map slot (the guardian is always present in the upstream
///   `AuthGuardedMultisig` component — there is no enable/disable selector)
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
        self.account
            .storage()
            .get_map_item(&slot_name, StorageMapKey::new(key))
            .ok()
    }

    /// Returns the multisig threshold from storage.
    pub fn threshold(&self) -> Result<u32> {
        let slot_value = self
            .get_item_by_name(multisig_threshold_config_slot())
            .ok_or_else(|| {
                MultisigError::AccountStorage("threshold config slot not found".to_string())
            })?;

        Ok(slot_value[0].as_canonical_u64() as u32)
    }

    /// Returns the number of signers from storage.
    pub fn num_signers(&self) -> Result<u32> {
        let slot_value = self
            .get_item_by_name(multisig_threshold_config_slot())
            .ok_or_else(|| {
                MultisigError::AccountStorage("threshold config slot not found".to_string())
            })?;

        Ok(slot_value[1].as_canonical_u64() as u32)
    }

    /// Whether the account's code carries the auth procedure of the contract
    /// version this SDK pins (`ProcedureName::AuthTx`). False means the account
    /// was created from a different miden-standards release, so this SDK's
    /// hardcoded procedure roots do not describe it.
    pub fn is_pinned_contract_version(&self) -> bool {
        self.account
            .code()
            .has_procedure(ProcedureName::AuthTx.root())
    }

    /// Rejects accounts built from a different contract version before any
    /// procedure-root-keyed storage read. Without this, reads against such an
    /// account silently miss its stored overrides (the map is keyed by *its*
    /// roots, not this SDK's) and report the default threshold.
    fn assert_pinned_contract_version(&self) -> Result<()> {
        if self.is_pinned_contract_version() {
            return Ok(());
        }
        Err(MultisigError::UnsupportedContractVersion {
            account_id: self.account.id(),
        })
    }

    /// Returns the configured threshold override for a specific procedure, if present.
    pub fn procedure_threshold(&self, procedure: ProcedureName) -> Result<Option<u32>> {
        self.assert_pinned_contract_version()?;
        let value =
            self.get_map_item_by_name(multisig_procedure_thresholds_slot(), procedure.root());
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

    /// Returns the per-procedure threshold overrides whose effective signing
    /// ratio is diluted by growing the signer set to `new_num_signers`.
    ///
    /// Overrides are absolute signature counts, not ratios, and the on-chain
    /// `update_signers_and_threshold` procedure does not re-scale them: growing
    /// the approver set silently lowers every override's effective signing
    /// ratio (a 2-of-2 override becomes 2-of-n). Callers creating a proposal
    /// that grows the signer set should surface these overrides and suggest
    /// raising them via `update_procedure_threshold` alongside the growth.
    pub fn overrides_diluted_by_signer_growth(
        &self,
        new_num_signers: u32,
    ) -> Result<Vec<(ProcedureName, u32)>> {
        if new_num_signers <= self.num_signers()? {
            return Ok(Vec::new());
        }
        self.procedure_threshold_overrides()
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
            TransactionType::Custom => return self.threshold(),
        };

        self.effective_threshold_for_procedure(procedure)
    }

    /// Extracts cosigner commitments from signer public keys map slot.
    ///
    /// Returns a vector of commitment Words. Returns empty vector if
    /// the slot is empty or has no entries.
    pub fn cosigner_commitments(&self) -> Vec<Word> {
        self.extract_indexed_map_words(multisig_approver_pubkeys_slot())
    }

    fn extract_indexed_map_words(&self, slot_name: &str) -> Vec<Word> {
        let mut commitments = Vec::new();
        let Ok(slot_name) = StorageSlotName::new(slot_name) else {
            return commitments;
        };

        let mut index = 0u32;
        loop {
            let key = Word::from([index, 0, 0, 0]);
            match self
                .account
                .storage()
                .get_map_item(&slot_name, StorageMapKey::new(key))
            {
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

    /// Returns the GUARDIAN server commitment from GUARDIAN public key map slot.
    pub fn guardian_commitment(&self) -> Result<Word> {
        let key = Word::from([0u32, 0, 0, 0]);
        self.get_map_item_by_name(guardian_public_key_slot(), key)
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

        let slot_name =
            StorageSlotName::new(multisig_procedure_thresholds_slot()).map_err(|e| {
                MultisigError::AccountStorage(format!(
                    "invalid procedure threshold slot name: {}",
                    e
                ))
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
            .filter(|current| current.name().as_str() != multisig_procedure_thresholds_slot())
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
            .filter(|slot| slot.name().as_str() != multisig_approver_pubkeys_slot())
            .chain([signer_slot(
                multisig_approver_pubkeys_slot(),
                oz_commitments,
            )])
            .collect();
        let storage = AccountStorage::new(storage_slots).expect("valid storage");
        let account = Account::new_unchecked(id, vault, storage, code, nonce, seed);

        MultisigAccount::new(account)
    }

    /// An account built from a different contract version (here: `NoAuth` +
    /// `BasicWallet`, which lacks the pinned guarded-multisig auth procedure)
    /// must fail root-keyed threshold reads loudly instead of silently
    /// reporting the default threshold.
    #[test]
    fn procedure_threshold_rejects_foreign_contract_version() {
        use miden_protocol::account::AccountBuilder;
        use miden_standards::account::auth::NoAuth;
        use miden_standards::account::wallets::BasicWallet;

        let account = AccountBuilder::new([3u8; 32])
            .with_auth_component(NoAuth)
            .with_component(BasicWallet)
            .build_existing()
            .expect("account builds");
        let account = MultisigAccount::new(account);

        assert!(!account.is_pinned_contract_version());
        let err = account
            .procedure_threshold(ProcedureName::SendAsset)
            .unwrap_err();
        assert!(matches!(
            err,
            MultisigError::UnsupportedContractVersion { .. }
        ));
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
    fn overrides_diluted_by_signer_growth_lists_overrides_only_on_growth() {
        let account = build_test_account();
        let num_signers = account.num_signers().expect("num signers");

        let diluted = account
            .overrides_diluted_by_signer_growth(num_signers + 1)
            .expect("diluted overrides");
        assert_eq!(
            diluted,
            account
                .procedure_threshold_overrides()
                .expect("configured overrides"),
            "growth must report every configured override"
        );

        assert!(
            account
                .overrides_diluted_by_signer_growth(num_signers)
                .expect("same size")
                .is_empty(),
            "unchanged signer count must report nothing"
        );
        assert!(
            account
                .overrides_diluted_by_signer_growth(num_signers - 1)
                .expect("shrink")
                .is_empty(),
            "shrinking must report nothing"
        );
    }

    #[test]
    fn target_signer_count_grows_only_for_signer_set_changes() {
        use crate::proposal::TransactionType;

        let commitment = word(42);
        assert_eq!(
            TransactionType::add_cosigner(commitment).target_signer_count(3),
            Some(4)
        );
        assert_eq!(
            TransactionType::remove_cosigner(commitment).target_signer_count(3),
            Some(2)
        );
        assert_eq!(
            TransactionType::UpdateSigners {
                new_threshold: 2,
                signer_commitments: vec![word(1), word(2), word(3), word(4), word(5)],
            }
            .target_signer_count(3),
            Some(5)
        );
        assert_eq!(
            TransactionType::switch_guardian("https://g.example", commitment)
                .target_signer_count(3),
            None
        );
        assert_eq!(TransactionType::Custom.target_signer_count(3), None);
    }

    #[test]
    fn effective_threshold_for_transaction_maps_to_expected_procedures() {
        let account = build_test_account();
        let account_id =
            AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").expect("account id");

        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::P2ID {
                    recipient: account_id,
                    faucet_id: account_id,
                    amount: 10,
                    note_type: miden_protocol::note::NoteType::Public,
                })
                .expect("threshold"),
            1
        );
        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::ConsumeNotes {
                    note_ids: vec![NoteId::from_raw(word(5))],
                    metadata_version: None,
                    notes: Vec::new(),
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
        assert_eq!(
            account
                .effective_threshold_for_transaction(&TransactionType::Custom)
                .expect("threshold"),
            2,
            "custom proposals use the account default threshold"
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
