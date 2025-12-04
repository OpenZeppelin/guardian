//! Proposal types and utilities for multisig transactions.

use miden_objects::account::AccountId;
use miden_objects::transaction::TransactionSummary;
use miden_objects::{Felt, Word};
use private_state_manager_client::DeltaObject;
use private_state_manager_shared::FromJson;
use serde_json::Value;

use crate::error::{MultisigError, Result};

/// Status of a proposal in the signing workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalStatus {
    /// Proposal created, awaiting signatures.
    Pending {
        /// Number of signatures collected so far.
        signatures_collected: usize,
        /// Number of signatures required (threshold).
        signatures_required: usize,
        /// Commitment hex strings of signers who have signed.
        signers: Vec<String>,
    },
    /// All signatures collected, ready for finalization.
    Ready,
    /// Proposal has been finalized and submitted.
    Finalized,
}

impl ProposalStatus {
    /// Returns true if the proposal is ready for finalization.
    pub fn is_ready(&self) -> bool {
        matches!(self, ProposalStatus::Ready)
    }

    /// Returns true if the proposal is still pending signatures.
    pub fn is_pending(&self) -> bool {
        matches!(self, ProposalStatus::Pending { .. })
    }
}

/// Types of transactions supported by the multisig SDK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionType {
    /// Transfer assets to another account via P2ID.
    P2ID {
        recipient: AccountId,
        faucet_id: AccountId,
        amount: u64,
    },
    /// Add a new cosigner to the multisig.
    AddCosigner { new_commitment: Word },
    /// Remove an existing cosigner from the multisig.
    RemoveCosigner { commitment: Word },
    /// Switch to a different PSM server.
    SwitchPsm {
        new_endpoint: String,
        new_commitment: Word,
    },
    /// Update signers configuration (generic).
    UpdateSigners {
        new_threshold: u32,
        signer_commitments: Vec<Word>,
    },
    /// Unknown transaction type.
    Unknown,
}

/// Metadata needed to reconstruct and finalize a proposal.
#[derive(Debug, Clone, Default)]
pub struct ProposalMetadata {
    /// The raw transaction summary JSON.
    pub tx_summary_json: Option<Value>,
    /// New threshold (for signer updates).
    pub new_threshold: Option<u64>,
    /// Signer commitments as hex strings.
    pub signer_commitments_hex: Vec<String>,
    /// Salt used for transaction authentication.
    pub salt_hex: Option<String>,
}

impl ProposalMetadata {
    /// Converts salt hex to Word.
    pub fn salt(&self) -> Word {
        self.salt_hex
            .as_ref()
            .map(|s| hex_to_word(s))
            .unwrap_or_else(|| Word::from([Felt::new(0); 4]))
    }

    /// Converts signer commitments to Words.
    pub fn signer_commitments(&self) -> Vec<Word> {
        self.signer_commitments_hex
            .iter()
            .map(|h| hex_to_word(h))
            .collect()
    }
}

/// A proposal for a multisig transaction.
#[derive(Debug, Clone)]
pub struct Proposal {
    /// Unique identifier (tx_summary commitment hex).
    pub id: String,
    /// Account nonce at proposal creation.
    pub nonce: u64,
    /// Type of transaction.
    pub transaction_type: TransactionType,
    /// Current status.
    pub status: ProposalStatus,
    /// The transaction summary.
    pub tx_summary: TransactionSummary,
    /// Metadata for reconstruction.
    pub metadata: ProposalMetadata,
}

impl Proposal {
    /// Creates a Proposal from a PSM DeltaObject.
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

        // Extract metadata
        let metadata_obj = payload_json.get("metadata");

        let new_threshold = metadata_obj
            .and_then(|m| m.get("new_threshold"))
            .and_then(|v| v.as_u64());

        let signer_commitments_hex: Vec<String> = metadata_obj
            .and_then(|m| m.get("signer_commitments_hex"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let salt_hex = metadata_obj
            .and_then(|m| m.get("salt_hex"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let metadata = ProposalMetadata {
            tx_summary_json: Some(tx_summary_json.clone()),
            new_threshold,
            signer_commitments_hex: signer_commitments_hex.clone(),
            salt_hex,
        };

        // Determine transaction type
        let proposed_signers = metadata.signer_commitments();
        let transaction_type = if let Some(threshold) = new_threshold {
            determine_transaction_type(
                threshold as u32,
                current_threshold,
                current_signers,
                &proposed_signers,
            )
        } else {
            TransactionType::Unknown
        };

        // Count signatures from delta status
        let (signatures_collected, signers) = count_signatures_from_delta(delta);
        let threshold_for_status =
            new_threshold.map(|t| t as usize).unwrap_or(current_threshold as usize);

        let status =
            if signatures_collected >= threshold_for_status && threshold_for_status > 0 {
            ProposalStatus::Ready
        } else {
            ProposalStatus::Pending {
                signatures_collected,
                signatures_required: threshold_for_status,
                signers,
            }
        };

        // Compute proposal ID from tx_summary commitment
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

    /// Creates a new Proposal (used when creating proposals locally).
    pub fn new(
        tx_summary: TransactionSummary,
        nonce: u64,
        transaction_type: TransactionType,
        metadata: ProposalMetadata,
    ) -> Self {
        let commitment = tx_summary.to_commitment();
        let id = format!("0x{}", hex::encode(word_to_bytes(&commitment)));

        let signatures_required = metadata.signer_commitments_hex.len();

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

    /// Checks if a signer has already signed this proposal.
    pub fn has_signed(&self, signer_commitment_hex: &str) -> bool {
        match &self.status {
            ProposalStatus::Pending { signers, .. } => signers
                .iter()
                .any(|s| s.eq_ignore_ascii_case(signer_commitment_hex)),
            _ => false,
        }
    }

    /// Returns the number of signatures collected.
    pub fn signatures_collected(&self) -> usize {
        match &self.status {
            ProposalStatus::Pending {
                signatures_collected,
                ..
            } => *signatures_collected,
            ProposalStatus::Ready => self.metadata.signer_commitments_hex.len(),
            ProposalStatus::Finalized => self.metadata.signer_commitments_hex.len(),
        }
    }

    /// Returns the number of signatures required.
    pub fn signatures_required(&self) -> usize {
        match &self.status {
            ProposalStatus::Pending {
                signatures_required,
                ..
            } => *signatures_required,
            _ => self.metadata.signer_commitments_hex.len(),
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
        if let Some(new_commitment) =
            proposed_signers
                .iter()
                .find(|candidate| !current_signers.iter().any(|c| c == *candidate))
        {
            return TransactionType::AddCosigner {
                new_commitment: new_commitment.clone(),
            };
        }
    } else if proposed_signers.len() < current_signers.len() {
        if let Some(removed_commitment) =
            current_signers
                .iter()
                .find(|candidate| !proposed_signers.iter().any(|c| c == *candidate))
        {
            return TransactionType::RemoveCosigner {
                commitment: removed_commitment.clone(),
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
fn hex_to_word(hex: &str) -> Word {
    let hex = hex.strip_prefix("0x").unwrap_or(hex);
    let bytes = hex::decode(hex).unwrap_or_else(|_| vec![0u8; 32]);
    let mut word = [0u64; 4];
    for (i, chunk) in bytes.chunks(8).enumerate() {
        if i >= 4 {
            break;
        }
        let mut arr = [0u8; 8];
        let len = chunk.len().min(8);
        arr[..len].copy_from_slice(&chunk[..len]);
        word[i] = u64::from_le_bytes(arr);
    }
    Word::from(word.map(Felt::new))
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

    #[test]
    fn test_hex_to_word_roundtrip() {
        let original = "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
        let word = hex_to_word(original);
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
}
