//! Proposal types and utilities for multisig transactions.

use std::collections::HashSet;

use miden_protocol::account::AccountId;
use miden_protocol::note::NoteId;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::{Felt, Word};
use private_state_manager_client::DeltaObject;
use private_state_manager_shared::FromJson;
use private_state_manager_shared::ProposalSignature;
use serde_json::Value;

use crate::error::{MultisigError, Result};
use crate::keystore::word_from_hex;
use crate::payload::ProposalPayload;

/// Status of a proposal in the signing workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalStatus {
    Pending,
    Ready,
    Finalized,
}

impl ProposalStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, ProposalStatus::Ready)
    }

    pub fn is_pending(&self) -> bool {
        matches!(self, ProposalStatus::Pending)
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
    pub signers: Vec<String>,
}

impl ProposalMetadata {
    /// Converts salt hex to Word.
    pub fn salt(&self) -> Result<Word> {
        match &self.salt_hex {
            Some(value) => word_from_hex(value).map_err(MultisigError::InvalidConfig),
            None => Ok(Word::from([Felt::new(0); 4])),
        }
    }

    /// Converts signer commitments to Words.
    pub fn signer_commitments(&self) -> Result<Vec<Word>> {
        self.signer_commitments_hex
            .iter()
            .map(|hex| word_from_hex(hex).map_err(MultisigError::InvalidConfig))
            .collect()
    }

    /// Converts note ID hex strings to NoteIds.
    pub fn note_ids(&self) -> Result<Vec<NoteId>> {
        self.note_ids_hex
            .iter()
            .map(|hex| {
                let word = word_from_hex(hex).map_err(MultisigError::InvalidConfig)?;
                Ok(NoteId::from_raw(word))
            })
            .collect()
    }

    fn to_transaction_type(&self, proposal_type: &str) -> Result<TransactionType> {
        if proposal_type.is_empty() {
            return Err(MultisigError::InvalidConfig(
                "proposal metadata.proposal_type is required".to_string(),
            ));
        }

        match proposal_type {
            "consume_notes" => {
                if self.note_ids_hex.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "consume_notes proposal requires metadata.note_ids".to_string(),
                    ));
                }
                Ok(TransactionType::ConsumeNotes {
                    note_ids: self.note_ids()?,
                })
            }
            "p2id" => {
                let recipient_str = self.recipient_hex.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "p2id proposal requires metadata.recipient_id".to_string(),
                    )
                })?;
                let faucet_str = self.faucet_id_hex.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "p2id proposal requires metadata.faucet_id".to_string(),
                    )
                })?;
                let parsed_amount = self.amount.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "p2id proposal requires metadata.amount".to_string(),
                    )
                })?;
                let recipient = AccountId::from_hex(recipient_str).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid recipient: {}", e))
                })?;
                let faucet_id = AccountId::from_hex(faucet_str).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid faucet_id: {}", e))
                })?;
                Ok(TransactionType::P2ID {
                    recipient,
                    faucet_id,
                    amount: parsed_amount,
                })
            }
            "switch_psm" => {
                let pubkey_hex = self.new_psm_pubkey_hex.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "switch_psm proposal requires metadata.new_psm_pubkey".to_string(),
                    )
                })?;
                let endpoint = self.new_psm_endpoint.as_ref().ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "switch_psm proposal requires metadata.new_psm_endpoint".to_string(),
                    )
                })?;
                let new_commitment =
                    word_from_hex(pubkey_hex).map_err(MultisigError::InvalidConfig)?;
                Ok(TransactionType::SwitchPsm {
                    new_endpoint: endpoint.clone(),
                    new_commitment,
                })
            }
            "add_signer" => {
                let threshold = self.new_threshold.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "add_signer proposal requires metadata.target_threshold".to_string(),
                    )
                })?;
                let proposed_signers = self.signer_commitments()?;
                if proposed_signers.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "add_signer proposal requires metadata.signer_commitments".to_string(),
                    ));
                }
                Ok(TransactionType::UpdateSigners {
                    new_threshold: threshold as u32,
                    signer_commitments: proposed_signers,
                })
            }
            "remove_signer" => {
                let threshold = self.new_threshold.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "remove_signer proposal requires metadata.target_threshold".to_string(),
                    )
                })?;
                let proposed_signers = self.signer_commitments()?;
                if proposed_signers.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "remove_signer proposal requires metadata.signer_commitments".to_string(),
                    ));
                }
                Ok(TransactionType::UpdateSigners {
                    new_threshold: threshold as u32,
                    signer_commitments: proposed_signers,
                })
            }
            "change_threshold" => {
                let threshold = self.new_threshold.ok_or_else(|| {
                    MultisigError::InvalidConfig(
                        "change_threshold proposal requires metadata.target_threshold".to_string(),
                    )
                })?;
                let proposed_signers = self.signer_commitments()?;
                if proposed_signers.is_empty() {
                    return Err(MultisigError::InvalidConfig(
                        "change_threshold proposal requires metadata.signer_commitments"
                            .to_string(),
                    ));
                }
                Ok(TransactionType::UpdateSigners {
                    new_threshold: threshold as u32,
                    signer_commitments: proposed_signers,
                })
            }
            other => Err(MultisigError::UnknownTransactionType(other.to_string())),
        }
    }
}

/// A proposal for a multisig transaction.
#[derive(Debug, Clone)]
pub struct ProposalSignatureEntry {
    pub signer_commitment: String,
    pub signature_hex: String,
}

/// A proposal for a multisig transaction.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: String,
    pub nonce: u64,
    pub transaction_type: TransactionType,
    pub status: ProposalStatus,
    pub tx_summary: TransactionSummary,
    pub signatures: Vec<ProposalSignatureEntry>,
    pub metadata: ProposalMetadata,
}

impl Proposal {
    pub fn from(delta: &DeltaObject) -> Result<Self> {
        let payload: ProposalPayload = serde_json::from_str(&delta.delta_payload)?;

        let tx_summary = TransactionSummary::from_json(&payload.tx_summary).map_err(|e| {
            MultisigError::MidenClient(format!("failed to parse tx_summary: {}", e))
        })?;

        let metadata_payload = payload.metadata.clone().ok_or_else(|| {
            MultisigError::InvalidConfig("proposal is missing metadata".to_string())
        })?;
        let proposal_type = metadata_payload.proposal_type.clone();
        let required_signatures = metadata_payload.required_signatures.ok_or_else(|| {
            MultisigError::InvalidConfig(
                "proposal metadata.required_signatures is required".to_string(),
            )
        })?;
        let required_signatures: usize = usize::try_from(required_signatures).map_err(|_| {
            MultisigError::InvalidConfig(
                "proposal metadata.required_signatures exceeds platform limits".to_string(),
            )
        })?;

        let new_threshold = metadata_payload.target_threshold;
        let signer_commitments_hex = metadata_payload.signer_commitments;
        let salt_hex = metadata_payload.salt;
        let recipient_hex = metadata_payload.recipient_id;
        let faucet_id_hex = metadata_payload.faucet_id;
        let amount = metadata_payload.amount.as_deref().map(|value| {
            value.parse::<u64>().map_err(|e| {
                MultisigError::InvalidConfig(format!(
                    "invalid metadata.amount value '{}': {}",
                    value, e
                ))
            })
        });
        let amount = match amount {
            Some(parsed) => Some(parsed?),
            None => None,
        };
        let note_ids_hex = metadata_payload.note_ids;

        let new_psm_pubkey_hex = metadata_payload.new_psm_pubkey;
        let new_psm_endpoint = metadata_payload.new_psm_endpoint;

        let mut metadata = ProposalMetadata {
            tx_summary_json: Some(payload.tx_summary.clone()),
            new_threshold,
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex,
            recipient_hex: recipient_hex.clone(),
            faucet_id_hex: faucet_id_hex.clone(),
            amount,
            note_ids_hex: note_ids_hex.clone(),
            new_psm_pubkey_hex: new_psm_pubkey_hex.clone(),
            new_psm_endpoint: new_psm_endpoint.clone(),
            required_signatures: Some(required_signatures),
            signers: Vec::new(),
        };
        let transaction_type = metadata.to_transaction_type(&proposal_type)?;

        let signatures: Vec<ProposalSignatureEntry> = payload
            .signatures
            .iter()
            .map(|signature| {
                let signature_hex = match &signature.signature {
                    ProposalSignature::Falcon { signature } => signature.clone(),
                };
                ProposalSignatureEntry {
                    signer_commitment: signature.signer_id.clone(),
                    signature_hex,
                }
            })
            .collect();

        let mut seen_signers = HashSet::new();
        metadata.signers = signatures
            .iter()
            .filter_map(|signature| {
                let signer_id = signature.signer_commitment.clone();
                if seen_signers.insert(signer_id.to_lowercase()) {
                    Some(signer_id)
                } else {
                    None
                }
            })
            .collect();

        let commitment = tx_summary.to_commitment();
        let id = format!("0x{}", hex::encode(word_to_bytes(&commitment)));

        let mut proposal = Proposal {
            id,
            nonce: delta.nonce,
            transaction_type,
            status: ProposalStatus::Pending,
            tx_summary,
            signatures,
            metadata,
        };
        proposal.refresh_status();
        Ok(proposal)
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

        let signatures_required = metadata
            .required_signatures
            .unwrap_or(metadata.signer_commitments_hex.len());
        metadata
            .required_signatures
            .get_or_insert(signatures_required);

        let mut proposal = Self {
            id,
            nonce,
            transaction_type,
            status: ProposalStatus::Pending,
            tx_summary,
            signatures: Vec::new(),
            metadata,
        };
        proposal.refresh_status();
        proposal
    }

    pub fn has_signed(&self, signer_commitment_hex: &str) -> bool {
        self.metadata
            .signers
            .iter()
            .any(|s| s.eq_ignore_ascii_case(signer_commitment_hex))
    }

    pub fn signatures_collected(&self) -> usize {
        self.metadata.signers.len()
    }

    pub fn signatures_required(&self) -> usize {
        self.metadata
            .required_signatures
            .unwrap_or(self.metadata.signer_commitments_hex.len())
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
        if !self.status.is_pending() {
            return Vec::new();
        }

        let signed: HashSet<_> = self
            .metadata
            .signers
            .iter()
            .map(|s| s.to_lowercase())
            .collect();

        self.metadata
            .signer_commitments_hex
            .iter()
            .filter(|c| !signed.contains(&c.to_lowercase()))
            .cloned()
            .collect()
    }

    fn refresh_status(&mut self) {
        let signatures_required = self.signatures_required();
        self.status =
            if self.metadata.signers.len() >= signatures_required && signatures_required > 0 {
                ProposalStatus::Ready
            } else {
                ProposalStatus::Pending
            };
    }
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
    use miden_protocol::FieldElement;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_protocol::transaction::{InputNotes, OutputNotes};

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
    fn test_word_from_hex_roundtrip() {
        let original = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let word = word_from_hex(original).expect("hex should decode");
        let bytes = word_to_bytes(&word);
        let result = format!("0x{}", hex::encode(bytes));
        assert_eq!(original, result);
    }

    #[test]
    fn test_word_from_hex_rejects_non_canonical_field_element() {
        let invalid = format!("0x{}{}", "ff".repeat(8), "00".repeat(24));
        let err = word_from_hex(&invalid).expect_err("non-canonical field element should fail");
        assert!(err.contains("invalid field element"));
    }

    #[test]
    fn test_proposal_status_checks() {
        let pending = ProposalStatus::Pending;
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
        let note_id = NoteId::from_raw(Word::default());
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
    fn test_transaction_type_switch_psm_rejects_non_canonical_commitment() {
        let metadata = ProposalMetadata {
            new_psm_pubkey_hex: Some(format!("0x{}{}", "ff".repeat(8), "00".repeat(24))),
            new_psm_endpoint: Some("http://new-psm.example.com".to_string()),
            ..Default::default()
        };

        let err = metadata
            .to_transaction_type("switch_psm")
            .expect_err("non-canonical PSM commitment should be rejected");
        assert!(err.to_string().contains("invalid field element"));
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
        let pending = ProposalStatus::Pending;

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: pending,
            tx_summary: create_test_tx_summary(),
            signatures: Vec::new(),
            metadata: ProposalMetadata {
                required_signatures: Some(3),
                signers: vec!["0xabc".to_string()],
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
        let pending = ProposalStatus::Pending;

        let proposal = Proposal {
            id: "0x123".to_string(),
            nonce: 1,
            transaction_type: TransactionType::add_cosigner(Word::default()),
            status: pending,
            tx_summary: create_test_tx_summary(),
            signatures: Vec::new(),
            metadata: ProposalMetadata {
                signers: vec!["0xABC".to_string()], // uppercase to test case-insensitivity
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
            signatures: Vec::new(),
            metadata: ProposalMetadata {
                required_signatures: Some(2),
                signers: vec!["0xabc".to_string(), "0xdef".to_string()],
                ..Default::default()
            },
        };

        assert_eq!(proposal.signatures_needed(), 0);
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
    fn test_metadata_salt_rejects_non_canonical_field_element() {
        let metadata = ProposalMetadata {
            salt_hex: Some(format!("0x{}{}", "ff".repeat(8), "00".repeat(24))),
            ..Default::default()
        };

        let err = metadata
            .salt()
            .expect_err("non-canonical salt should be rejected");
        assert!(err.to_string().contains("invalid field element"));
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
