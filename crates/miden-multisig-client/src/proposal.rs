//! Proposal types and utilities for multisig transactions.

use std::collections::HashSet;

use miden_protocol::account::AccountId;
use miden_protocol::note::NoteId;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::{Felt, Word};
use private_state_manager_client::DeltaObject;
use private_state_manager_shared::FromJson;
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

    pub(crate) fn proposal_type(&self) -> Option<&'static str> {
        match self {
            Self::P2ID { .. } => Some("p2id"),
            Self::ConsumeNotes { .. } => Some("consume_notes"),
            Self::AddCosigner { .. } => Some("add_signer"),
            Self::RemoveCosigner { .. } => Some("remove_signer"),
            Self::SwitchPsm { .. } => Some("switch_psm"),
            Self::UpdateSigners { .. } => None,
        }
    }

    /// Returns a stable transaction type name for diagnostics and flow checks.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::P2ID { .. } => "P2ID",
            Self::ConsumeNotes { .. } => "ConsumeNotes",
            Self::AddCosigner { .. } => "AddCosigner",
            Self::RemoveCosigner { .. } => "RemoveCosigner",
            Self::SwitchPsm { .. } => "SwitchPsm",
            Self::UpdateSigners { .. } => "UpdateSigners",
        }
    }

    /// Returns true when execution can proceed without a PSM acknowledgment.
    pub fn supports_offline_execution(&self) -> bool {
        matches!(self, Self::SwitchPsm { .. })
    }

    /// Returns true when execution requires a PSM acknowledgment signature.
    pub fn requires_psm_ack(&self) -> bool {
        !self.supports_offline_execution()
    }
}

/// Metadata needed to reconstruct and finalize a proposal.
#[derive(Debug, Clone, Default)]
pub struct ProposalMetadata {
    pub tx_summary_json: Option<Value>,
    pub proposal_type: Option<String>,
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

    pub(crate) fn to_transaction_type(&self, proposal_type: &str) -> Result<TransactionType> {
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
                let new_commitment = word_from_hex(pubkey_hex).map_err(MultisigError::InvalidConfig)?;
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
        let payload: ProposalPayload = serde_json::from_str(&delta.delta_payload)?;

        let tx_summary = TransactionSummary::from_json(&payload.tx_summary).map_err(|e| {
            MultisigError::MidenClient(format!("failed to parse tx_summary: {}", e))
        })?;

        let metadata_payload = payload.metadata.clone().unwrap_or_default();
        let new_threshold = metadata_payload.target_threshold;
        let signer_commitments_hex = metadata_payload.signer_commitments;
        let salt_hex = metadata_payload.salt;
        let recipient_hex = metadata_payload.recipient_id;
        let faucet_id_hex = metadata_payload.faucet_id;
        let amount = metadata_payload
            .amount
            .as_deref()
            .and_then(|value| value.parse().ok());
        let note_ids_hex = metadata_payload.note_ids;

        let parsed_note_ids: Vec<NoteId> = note_ids_hex
            .iter()
            .map(|hex| {
                let word = word_from_hex(hex).map_err(MultisigError::InvalidConfig)?;
                Ok(NoteId::from_raw(word))
            })
            .collect::<Result<_>>()?;

        let new_psm_pubkey_hex = metadata_payload.new_psm_pubkey;
        let new_psm_endpoint = metadata_payload.new_psm_endpoint;

        let proposal_type = if metadata_payload.proposal_type.is_empty() {
            None
        } else {
            Some(metadata_payload.proposal_type.clone())
        };

        let mut metadata = ProposalMetadata {
            tx_summary_json: Some(payload.tx_summary.clone()),
            proposal_type: proposal_type.clone(),
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
            signers: Vec::new(),
        };

        let transaction_type = if let Some(proposal_type) = metadata.proposal_type.as_deref() {
            metadata.to_transaction_type(proposal_type)?
        } else {
            if !parsed_note_ids.is_empty() {
                TransactionType::ConsumeNotes {
                    note_ids: parsed_note_ids,
                }
            } else if let (Some(recipient_str), Some(faucet_str), Some(amt)) =
                (&recipient_hex, &faucet_id_hex, amount)
            {
                let recipient = AccountId::from_hex(recipient_str).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid recipient: {}", e))
                })?;
                let faucet_id = AccountId::from_hex(faucet_str).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid faucet_id: {}", e))
                })?;
                TransactionType::P2ID {
                    recipient,
                    faucet_id,
                    amount: amt,
                }
            } else if let (Some(pubkey_hex), Some(endpoint)) =
                (&new_psm_pubkey_hex, &new_psm_endpoint)
            {
                let new_commitment =
                    word_from_hex(pubkey_hex).map_err(MultisigError::InvalidConfig)?;
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
            }
        };

        let mut seen_signers = HashSet::new();
        metadata.signers = payload
            .signatures
            .iter()
            .filter_map(|signature| {
                let signer_id = signature.signer_id.clone();
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
        if metadata.proposal_type.is_none() {
            metadata.proposal_type = transaction_type.proposal_type().map(str::to_string);
        }

        let mut proposal = Self {
            id,
            nonce,
            transaction_type,
            status: ProposalStatus::Pending,
            tx_summary,
            metadata,
        };
        proposal.refresh_status();
        proposal
    }

    pub fn set_required_signatures(&mut self, required_signatures: usize) {
        self.metadata.required_signatures = Some(required_signatures);
        self.refresh_status();
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
    fn test_transaction_type_requires_psm_ack() {
        let recipient = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594170").unwrap();
        let faucet_id = AccountId::from_hex("0x7bfb0f38b0fafa103f86a805594171").unwrap();

        assert!(TransactionType::transfer(recipient, faucet_id, 1).requires_psm_ack());
        assert!(
            !TransactionType::switch_psm("http://new-psm.example.com", Word::default())
                .requires_psm_ack()
        );
    }

    #[test]
    fn test_transaction_type_supports_offline_execution() {
        let note_id = NoteId::from_raw(Word::default());
        assert!(!TransactionType::consume_notes(vec![note_id]).supports_offline_execution());
        assert!(
            TransactionType::switch_psm("http://new-psm.example.com", Word::default())
                .supports_offline_execution()
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
            metadata: ProposalMetadata {
                required_signatures: Some(2),
                signers: vec!["0xabc".to_string(), "0xdef".to_string()],
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
