//! Proposal types and utilities for multisig transactions.

use miden_objects::account::AccountId;
use miden_objects::note::NoteId;
use miden_objects::transaction::TransactionSummary;
use miden_objects::{Felt, Word};
use private_state_manager_client::DeltaObject;
use private_state_manager_shared::FromJson;
use serde_json::Value;

use crate::error::{MultisigError, Result};

/// Status of a proposal in the signing workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalStatus {
    Pending {
        signatures_collected: usize,
        signatures_required: usize,
        signers: Vec<String>,
    },
    Ready,
    Finalized,
}

impl ProposalStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, ProposalStatus::Ready)
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ProposalStatus::Pending { .. })
    }
}

/// Types of transactions supported by the multisig SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionType {
    P2ID {
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
    },
    ConsumeNotes {
        note_ids: Vec<NoteId>,
    },
    AddCosigner {
        new_commitment: Word,
    },
    RemoveCosigner {
        commitment: Word,
    },
    SwitchPsm {
        new_endpoint: String,
        new_commitment: Word,
    },
    UpdateSigners {
        new_threshold: u32,
        signer_commitments: Vec<Word>,
    },
}

impl TransactionType {
    /// Creates a P2ID transfer transaction.
    pub fn transfer(recipient: AccountId, faucet_id: AccountId, amount: u64) -> Self {
        Self::P2ID {
            recipient,
            faucet_id,
            amount,
        }
    }

    /// Creates a ConsumeNotes transaction.
    pub fn consume_notes(note_ids: Vec<NoteId>) -> Self {
        Self::ConsumeNotes { note_ids }
    }

    /// Creates an AddCosigner transaction.
    pub fn add_cosigner(new_commitment: Word) -> Self {
        Self::AddCosigner { new_commitment }
    }

    /// Creates a RemoveCosigner transaction.
    pub fn remove_cosigner(commitment: Word) -> Self {
        Self::RemoveCosigner { commitment }
    }

    /// Creates a SwitchPsm transaction.
    pub fn switch_psm(new_endpoint: impl Into<String>, new_commitment: Word) -> Self {
        Self::SwitchPsm {
            new_endpoint: new_endpoint.into(),
            new_commitment,
        }
    }

    /// Creates an UpdateSigners transaction.
    pub fn update_signers(new_threshold: u32, signer_commitments: Vec<Word>) -> Self {
        Self::UpdateSigners {
            new_threshold,
            signer_commitments,
        }
    }
}

/// Metadata needed to reconstruct and finalize a proposal.
#[derive(Debug, Clone, Default)]
pub struct ProposalMetadata {
    pub tx_summary_json: Option<Value>,
    pub new_threshold: Option<u64>,
    pub signer_commitments_hex: Vec<String>,
    pub salt_hex: Option<String>,

    pub recipient_hex: Option<String>,
    pub faucet_id_hex: Option<String>,
    pub amount: Option<u64>,

    pub note_ids_hex: Vec<String>,

    pub new_psm_pubkey_hex: Option<String>,
    pub new_psm_endpoint: Option<String>,

    pub required_signatures: Option<usize>,
    pub collected_signatures: Option<usize>,
}

impl ProposalMetadata {
    /// Converts salt hex to Word.
    pub fn salt(&self) -> Result<Word> {
        match &self.salt_hex {
            Some(value) => hex_to_word(value),
            None => Ok(Word::from([Felt::new(0); 4])),
        }
    }

    /// Converts signer commitments to Words.
    pub fn signer_commitments(&self) -> Result<Vec<Word>> {
        self.signer_commitments_hex
            .iter()
            .map(|h| hex_to_word(h))
            .collect()
    }

    /// Converts note ID hex strings to NoteIds.
    pub fn note_ids(&self) -> Result<Vec<NoteId>> {
        self.note_ids_hex
            .iter()
            .map(|hex| Ok(NoteId::from(hex_to_word(hex)?)))
            .collect()
    }
}

/// A proposal for a multisig transaction.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: String,
    pub nonce: u64,
    pub transaction_type: TransactionType,
    pub status: ProposalStatus,
    pub tx_summary: TransactionSummary,
    pub metadata: ProposalMetadata,
}

impl Proposal {
    pub fn from(
        delta: &DeltaObject,
        current_threshold: u32,
        current_signers: &[Word],
    ) -> Result<Self> {
        let payload_json: Value = serde_json::from_str(&delta.delta_payload)?;

        let tx_summary_json = payload_json.get("tx_summary").ok_or_else(|| {
            MultisigError::InvalidConfig("missing tx_summary in delta".to_string())
        })?;

        let tx_summary = TransactionSummary::from_json(tx_summary_json).map_err(|e| {
            MultisigError::MidenClient(format!("failed to parse tx_summary: {}", e))
        })?;

        let metadata_obj = payload_json.get("metadata");

        let new_threshold = metadata_obj
            .and_then(|m| m.get("target_threshold"))
            .and_then(|v| v.as_u64());

        let signer_commitments_hex: Vec<String> = metadata_obj
            .and_then(|m| m.get("signer_commitments"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let salt_hex = metadata_obj
            .and_then(|m| m.get("salt"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Extract P2ID fields
        let recipient_hex = metadata_obj
            .and_then(|m| m.get("recipient_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let faucet_id_hex = metadata_obj
            .and_then(|m| m.get("faucet_id"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let amount = metadata_obj.and_then(|m| m.get("amount")).and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        });

        let note_ids_hex: Vec<String> = metadata_obj
            .and_then(|m| m.get("note_ids"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let parsed_note_ids: Vec<NoteId> = note_ids_hex
            .iter()
            .map(|hex| Ok(NoteId::from(hex_to_word(hex)?)))
            .collect::<Result<_>>()?;

        let new_psm_pubkey_hex = metadata_obj
            .and_then(|m| m.get("new_psm_pubkey"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let new_psm_endpoint = metadata_obj
            .and_then(|m| m.get("new_psm_endpoint"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let mut metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary_json.clone()),
            new_threshold,
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex,
            recipient_hex: recipient_hex.clone(),
            faucet_id_hex: faucet_id_hex.clone(),
            amount,
            note_ids_hex: note_ids_hex.clone(),
            new_psm_pubkey_hex: new_psm_pubkey_hex.clone(),
            new_psm_endpoint: new_psm_endpoint.clone(),
            required_signatures: Some(current_threshold as usize),
            collected_signatures: None,
        };

        let transaction_type = if !parsed_note_ids.is_empty() {
            TransactionType::ConsumeNotes {
                note_ids: parsed_note_ids,
            }
        } else if let (Some(recipient_str), Some(faucet_str), Some(amt)) =
            (&recipient_hex, &faucet_id_hex, amount)
        {
            let recipient = AccountId::from_hex(recipient_str)
                .map_err(|e| MultisigError::InvalidConfig(format!("invalid recipient: {}", e)))?;
            let faucet_id = AccountId::from_hex(faucet_str)
                .map_err(|e| MultisigError::InvalidConfig(format!("invalid faucet_id: {}", e)))?;
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount: amt,
            }
        } else if let (Some(pubkey_hex), Some(endpoint)) = (&new_psm_pubkey_hex, &new_psm_endpoint)
        {
            let new_commitment = hex_to_word(pubkey_hex)?;
            TransactionType::SwitchPsm {
                new_endpoint: endpoint.clone(),
                new_commitment,
            }
        } else if let Some(threshold) = new_threshold {
            let proposed_signers = metadata.signer_commitments()?;
            determine_transaction_type(
                threshold as u32,
                current_threshold,
                current_signers,
                &proposed_signers,
            )
        } else {
            return Err(MultisigError::UnknownTransactionType(
                "could not determine transaction type from proposal metadata".to_string(),
            ));
        };

        let (signatures_collected, signers) = count_signatures_from_delta(delta);
        let signatures_required = current_threshold as usize;
        metadata.collected_signatures = Some(signatures_collected);

        let status = if signatures_collected >= signatures_required && signatures_required > 0 {
            ProposalStatus::Ready
        } else {
            ProposalStatus::Pending {
                signatures_collected,
                signatures_required,
                signers,
            }
        };

        let commitment = tx_summary.to_commitment();
        let id = format!("0x{}", hex::encode(word_to_bytes(&commitment)));

        Ok(Proposal {
            id,
            nonce: delta.nonce,
            transaction_type,
            status,
            tx_summary,
            metadata,
        })
    }

    /// Creates a new Proposal
    pub fn new(
        tx_summary: TransactionSummary,
        nonce: u64,
        transaction_type: TransactionType,
        mut metadata: ProposalMetadata,
    ) -> Self {
        let commitment = tx_summary.to_commitment();
        let id = format!("0x{}", hex::encode(word_to_bytes(&commitment)));

        let signatures_required = metadata.signer_commitments_hex.len();
        metadata
            .required_signatures
            .get_or_insert(signatures_required);
        metadata.collected_signatures.get_or_insert(0);

        Self {
            id,
            nonce,
            transaction_type,
            status: ProposalStatus::Pending {
                signatures_collected: 0,
                signatures_required,
                signers: Vec::new(),
            },
            tx_summary,
            metadata,
        }
    }

    pub fn has_signed(&self, signer_commitment_hex: &str) -> bool {
        match &self.status {
            ProposalStatus::Pending { signers, .. } => signers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(signer_commitment_hex)),
            _ => false,
        }
    }

    pub fn signatures_collected(&self) -> usize {
        match &self.status {
            ProposalStatus::Pending {
                signatures_collected,
                ..
            } => *signatures_collected,
            ProposalStatus::Ready | ProposalStatus::Finalized => self
                .metadata
                .collected_signatures
                .or(self.metadata.required_signatures)
                .unwrap_or(self.metadata.signer_commitments_hex.len()),
        }
    }

    pub fn signatures_required(&self) -> usize {
        match &self.status {
            ProposalStatus::Pending {
                signatures_required,
                ..
            } => *signatures_required,
            _ => self
                .metadata
                .required_signatures
                .unwrap_or(self.metadata.signer_commitments_hex.len()),
        }
    }

    pub fn signature_counts(&self) -> (usize, usize) {
        (self.signatures_collected(), self.signatures_required())
    }

    pub fn signatures_needed(&self) -> usize {
        self.signatures_required()
            .saturating_sub(self.signatures_collected())
    }

    /// Returns the commitment hex strings of signers who haven't signed yet.
    pub fn missing_signers(&self) -> Vec<String> {
        match &self.status {
            ProposalStatus::Pending { signers, .. } => {
                let signed: std::collections::HashSet<_> =
                    signers.iter().map(|s| s.to_lowercase()).collect();

                self.metadata
                    .signer_commitments_hex
                    .iter()
                    .filter(|c| !signed.contains(&c.to_lowercase()))
                    .cloned()
                    .collect()
            }
            // Ready/Finalized proposals have no missing signers
            ProposalStatus::Ready | ProposalStatus::Finalized => Vec::new(),
        }
    }
}

/// Counts signatures from a DeltaObject's status.
fn count_signatures_from_delta(delta: &DeltaObject) -> (usize, Vec<String>) {
    if let Some(ref status) = delta.status
        && let Some(ref status_oneof) = status.status
    {
        use private_state_manager_client::delta_status::Status;
        if let Status::Pending(pending) = status_oneof {
            let signers: Vec<String> = pending
                .cosigner_sigs
                .iter()
                .map(|sig| sig.signer_id.clone())
                .collect();
            return (signers.len(), signers);
        }
    }
    (0, Vec::new())
}

fn determine_transaction_type(
    proposed_threshold: u32,
    current_threshold: u32,
    current_signers: &[Word],
    proposed_signers: &[Word],
) -> TransactionType {
    if proposed_signers.len() > current_signers.len() {
        if let Some(new_commitment) = proposed_signers
            .iter()
            .find(|candidate| !current_signers.iter().any(|c| c == *candidate))
        {
            return TransactionType::AddCosigner {
                new_commitment: *new_commitment,
            };
        }
    } else if proposed_signers.len() < current_signers.len() {
        if let Some(removed_commitment) = current_signers
            .iter()
            .find(|candidate| !proposed_signers.iter().any(|c| c == *candidate))
        {
            return TransactionType::RemoveCosigner {
                commitment: *removed_commitment,
            };
        }
    } else if proposed_threshold != current_threshold {
        return TransactionType::UpdateSigners {
            new_threshold: proposed_threshold,
            signer_commitments: proposed_signers.to_vec(),
        };
    }

    TransactionType::UpdateSigners {
        new_threshold: proposed_threshold,
        signer_commitments: proposed_signers.to_vec(),
    }
}

/// Converts a hex string to Word.
fn hex_to_word(hex: &str) -> Result<Word> {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    let bytes = hex::decode(hex).map_err(|e| {
        MultisigError::InvalidConfig(format!("invalid hex string '{}': {}", hex, e))
    })?;

    if bytes.len() != 32 {
        return Err(MultisigError::InvalidConfig(format!(
            "invalid word length for '{}': expected 32 bytes, got {}",
            hex,
            bytes.len()
        )));
    }

    let mut word = [0u64; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(chunk);
        word[i] = u64::from_le_bytes(arr);
    }
    Ok(Word::from(word.map(Felt::new)))
}

/// Converts a Word to bytes.
fn word_to_bytes(word: &Word) -> Vec<u8> {
    word.iter()
        .flat_map(|felt| felt.as_int().to_le_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::FieldElement;
    use miden_objects::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_objects::transaction::{InputNotes, OutputNotes};

    fn create_test_tx_summary() -> TransactionSummary {
        // Use a minimal valid account ID
        let account_id = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap();
        let delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("Valid empty delta");

        TransactionSummary::new(
            delta,
            InputNotes::new(Vec::new()).unwrap(),
            OutputNotes::new(Vec::new()).unwrap(),
            Word::default(),
        )
    }

    #[test]
    fn test_hex_to_word_roundtrip() {
        let original = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let word = hex_to_word(original).expect("hex should decode");
        let bytes = word_to_bytes(&word);
        let result = format!("0x{}", hex::encode(bytes));
        assert_eq!(original, result);
    }

    #[test]
    fn test_proposal_status_checks() {
        let pending = ProposalStatus::Pending {
            signatures_collected: 1,
            signatures_required: 2,
            signers: vec!["0xabc".to_string()],
        };
        assert!(pending.is_pending());
        assert!(!pending.is_ready());

        let ready = ProposalStatus::Ready;
        assert!(ready.is_ready());
        assert!(!ready.is_pending());
    }

    #[test]
    fn test_transaction_type_transfer() {
        // Use valid Miden AccountId format
        let recipient = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap();
        let faucet_id = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594171").unwrap();
        let amount = 1000u64;

        let tx = TransactionType::transfer(recipient, faucet_id, amount);

        assert_eq!(
            tx,
            TransactionType::P2ID {
                recipient,
                faucet_id,
                amount
            }
        );
    }

    #[test]
    fn test_transaction_type_consume_notes() {
        let note_id = NoteId::from(Word::default());
        let tx = TransactionType::consume_notes(vec![note_id]);

        assert_eq!(
            tx,
            TransactionType::ConsumeNotes {
                note_ids: vec![note_id]
            }
        );
    }

    #[test]
    fn test_transaction_type_add_cosigner() {
        let commitment = Word::default();
        let tx = TransactionType::add_cosigner(commitment);

        assert_eq!(
            tx,
            TransactionType::AddCosigner {
                new_commitment: commitment
            }
        );
    }

    #[test]
    fn test_transaction_type_remove_cosigner() {
        let commitment = Word::default();
        let tx = TransactionType::remove_cosigner(commitment);

        assert_eq!(tx, TransactionType::RemoveCosigner { commitment });
    }

    #[test]
    fn test_transaction_type_switch_psm() {
        let endpoint = "http://new-psm.example.com";
        let commitment = Word::default();

        let tx = TransactionType::switch_psm(endpoint, commitment);

        assert_eq!(
            tx,
            TransactionType::SwitchPsm {
                new_endpoint: endpoint.to_string(),
                new_commitment: commitment
            }
        );
    }

    #[test]
    fn test_transaction_type_update_signers() {
        let threshold = 2u32;
        let signers = vec![Word::default()];

        let tx = TransactionType::update_signers(threshold, signers.clone());

        assert_eq!(
            tx,
            TransactionType::UpdateSigners {
                new_threshold: threshold,
                signer_commitments: signers
            }
        );
    }

    #[test]
    fn test_proposal_signature_counts() {
        let pending = ProposalStatus::Pending {
            signatures_collected: 1,
            signatures_required: 3,
            signers: vec!["0xabc".to_string()],
        };

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: pending,
            tx_summary: create_test_tx_summary(),
            metadata: ProposalMetadata {
                signer_commitments_hex: vec![
                    "0xabc".to_string(),
                    "0xdef".to_string(),
                    "0x123".to_string(),
                ],
                ..Default::default()
            },
        };

        assert_eq!(proposal.signature_counts(), (1, 3));
        assert_eq!(proposal.signatures_needed(), 2);
    }

    #[test]
    fn test_proposal_missing_signers() {
        let pending = ProposalStatus::Pending {
            signatures_collected: 1,
            signatures_required: 3,
            signers: vec!["0xABC".to_string()], // uppercase to test case-insensitivity
        };

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: pending,
            tx_summary: create_test_tx_summary(),
            metadata: ProposalMetadata {
                signer_commitments_hex: vec![
                    "0xabc".to_string(), // lowercase
                    "0xdef".to_string(),
                    "0x456".to_string(),
                ],
                ..Default::default()
            },
        };

        let missing = proposal.missing_signers();
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&"0xdef".to_string()));
        assert!(missing.contains(&"0x456".to_string()));
        // 0xabc should NOT be in missing (already signed)
        assert!(!missing.contains(&"0xabc".to_string()));
    }

    #[test]
    fn test_proposal_signatures_needed_when_ready() {
        let ready = ProposalStatus::Ready;

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: ready,
            tx_summary: create_test_tx_summary(),
            metadata: ProposalMetadata {
                required_signatures: Some(2),
                collected_signatures: Some(2),
                ..Default::default()
            },
        };

        assert_eq!(proposal.signatures_needed(), 0);
    }

    // ==================== determine_transaction_type tests ====================

    fn word_from_u64(v: u64) -> Word {
        [Felt::new(v), Felt::ZERO, Felt::ZERO, Felt::ZERO].into()
    }

    #[test]
    fn test_determine_add_cosigner() {
        let current = vec![word_from_u64(1), word_from_u64(2)];
        let proposed = vec![word_from_u64(1), word_from_u64(2), word_from_u64(3)];

        let result = determine_transaction_type(2, 2, &current, &proposed);

        match result {
            TransactionType::AddCosigner { new_commitment } => {
                assert_eq!(new_commitment, word_from_u64(3));
            }
            _ => panic!("expected AddCosigner, got {:?}", result),
        }
    }

    #[test]
    fn test_determine_remove_cosigner() {
        let current = vec![word_from_u64(1), word_from_u64(2), word_from_u64(3)];
        let proposed = vec![word_from_u64(1), word_from_u64(3)];

        let result = determine_transaction_type(2, 2, &current, &proposed);

        match result {
            TransactionType::RemoveCosigner { commitment } => {
                assert_eq!(commitment, word_from_u64(2));
            }
            _ => panic!("expected RemoveCosigner, got {:?}", result),
        }
    }

    #[test]
    fn test_determine_update_signers_threshold_change() {
        let signers = vec![word_from_u64(1), word_from_u64(2)];

        let result = determine_transaction_type(3, 2, &signers, &signers);

        match result {
            TransactionType::UpdateSigners { new_threshold, .. } => {
                assert_eq!(new_threshold, 3);
            }
            _ => panic!("expected UpdateSigners, got {:?}", result),
        }
    }

    #[test]
    fn test_determine_no_change_returns_update_signers() {
        let signers = vec![word_from_u64(1), word_from_u64(2)];

        // Same threshold, same signers → falls through to UpdateSigners
        let result = determine_transaction_type(2, 2, &signers, &signers);

        match result {
            TransactionType::UpdateSigners {
                new_threshold,
                signer_commitments,
            } => {
                assert_eq!(new_threshold, 2);
                assert_eq!(signer_commitments.len(), 2);
            }
            _ => panic!("expected UpdateSigners, got {:?}", result),
        }
    }

    // ==================== ProposalMetadata parser tests ====================

    #[test]
    fn test_metadata_salt_valid() {
        let metadata = ProposalMetadata {
            salt_hex: Some(
                "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20".to_string(),
            ),
            ..Default::default()
        };

        let salt = metadata.salt().expect("salt should parse");
        // Verify it's not the default Word
        assert_ne!(salt, Word::default());
    }

    #[test]
    fn test_metadata_salt_none_returns_default() {
        let metadata = ProposalMetadata::default();

        let salt = metadata.salt().expect("salt should return default");
        assert_eq!(salt, Word::default());
    }

    #[test]
    fn test_metadata_signer_commitments_valid() {
        let hex1 = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let hex2 = "0x2122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40";

        let metadata = ProposalMetadata {
            signer_commitments_hex: vec![hex1.to_string(), hex2.to_string()],
            ..Default::default()
        };

        let commitments = metadata.signer_commitments().expect("should parse");
        assert_eq!(commitments.len(), 2);
    }

    #[test]
    fn test_metadata_signer_commitments_invalid_hex() {
        let metadata = ProposalMetadata {
            signer_commitments_hex: vec!["not_valid_hex".to_string()],
            ..Default::default()
        };

        assert!(metadata.signer_commitments().is_err());
    }

    #[test]
    fn test_metadata_note_ids_valid() {
        // NoteId is 32 bytes = 64 hex chars
        let note_hex = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

        let metadata = ProposalMetadata {
            note_ids_hex: vec![note_hex.to_string()],
            ..Default::default()
        };

        let note_ids = metadata.note_ids().expect("should parse");
        assert_eq!(note_ids.len(), 1);
    }
}
