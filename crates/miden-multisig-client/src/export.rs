//! Export/import types for offline proposal sharing.
//!
//! This module provides types and utilities for exporting proposals to files
//! and importing them back. This enables offline sharing of proposals via
//! side channels (email, USB, etc.) when the PSM server is unavailable.
//!
//! # Export File Format
//!
//! Proposals are exported as JSON files with all the information needed to:
//! - Display proposal details to cosigners
//! - Add signatures offline
//! - Execute the transaction when ready
//!
//! # Workflow
//!
//! **Exporting (Proposer):**
//! 1. Create proposal via `propose_transaction()`
//! 2. Export via `export_proposal()` or `export_proposal_to_string()`
//! 3. Share file via side channel
//!
//! **Importing & Signing (Cosigner):**
//! 1. Receive file via side channel
//! 2. Import via `import_proposal()`
//! 3. Sign via `sign_imported_proposal()`
//! 4. Export updated proposal with new signature
//! 5. Share back to proposer or next cosigner
//!
//! **Executing (Any cosigner with enough signatures):**
//! 1. Import final proposal with all signatures
//! 2. Execute via `execute_imported_proposal()`

use miden_objects::account::AccountId;
use miden_objects::transaction::TransactionSummary;
use private_state_manager_shared::FromJson;
use serde::{Deserialize, Serialize};

use crate::error::{MultisigError, Result};
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

/// Current export format version.
pub const EXPORT_VERSION: u32 = 1;

/// Exported proposal for offline sharing.
///
/// Contains all the information needed to reconstruct, sign, and execute
/// a proposal without access to the PSM server.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExportedProposal {
    /// Format version for future compatibility.
    pub version: u32,

    /// Account ID this proposal belongs to.
    pub account_id: String,

    /// Proposal ID (commitment hex).
    pub id: String,

    /// Account nonce at proposal creation.
    pub nonce: u64,

    /// Transaction type identifier.
    pub transaction_type: String,

    /// Full transaction summary as JSON.
    pub tx_summary: serde_json::Value,

    /// Signatures collected (accumulates as proposal is passed between cosigners).
    #[serde(default)]
    pub signatures: Vec<ExportedSignature>,

    /// Threshold required for execution.
    pub signatures_required: usize,

    /// All metadata needed for reconstruction.
    pub metadata: ExportedMetadata,
}

/// A signature collected for an exported proposal.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExportedSignature {
    /// Signer's public key commitment (hex).
    pub signer_commitment: String,
    /// Falcon signature (hex).
    pub signature: String,
}

/// Metadata needed for proposal reconstruction.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ExportedMetadata {
    /// Salt used for transaction authentication (hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salt_hex: Option<String>,

    /// New threshold (for signer updates).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_threshold: Option<u64>,

    /// Signer commitments as hex strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_commitments_hex: Vec<String>,

    /// Recipient account ID as hex string (for P2ID transfers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_hex: Option<String>,

    /// Faucet ID as hex string (for P2ID transfers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faucet_id_hex: Option<String>,

    /// Amount to transfer (for P2ID transfers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,

    /// Note IDs to consume as hex strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note_ids_hex: Vec<String>,

    /// New PSM public key commitment as hex string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_psm_pubkey_hex: Option<String>,

    /// New PSM endpoint URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_psm_endpoint: Option<String>,
}

impl ExportedProposal {
    /// Creates an ExportedProposal from a Proposal and account ID.
    pub fn from_proposal(proposal: &Proposal, account_id: AccountId) -> Self {
        let tx_type_str = match &proposal.transaction_type {
            TransactionType::P2ID { .. } => "P2ID",
            TransactionType::ConsumeNotes { .. } => "ConsumeNotes",
            TransactionType::AddCosigner { .. } => "AddCosigner",
            TransactionType::RemoveCosigner { .. } => "RemoveCosigner",
            TransactionType::SwitchPsm { .. } => "SwitchPsm",
            TransactionType::UpdateSigners { .. } => "UpdateSigners",
            TransactionType::Unknown => "Unknown",
        };

        let signatures_required = proposal.signatures_required();

        // We only have signer IDs from status, not full signatures
        // The signatures will need to be provided separately when exporting from PSM
        let signatures = Vec::new();

        let metadata = ExportedMetadata {
            salt_hex: proposal.metadata.salt_hex.clone(),
            new_threshold: proposal.metadata.new_threshold,
            signer_commitments_hex: proposal.metadata.signer_commitments_hex.clone(),
            recipient_hex: proposal.metadata.recipient_hex.clone(),
            faucet_id_hex: proposal.metadata.faucet_id_hex.clone(),
            amount: proposal.metadata.amount,
            note_ids_hex: proposal.metadata.note_ids_hex.clone(),
            new_psm_pubkey_hex: proposal.metadata.new_psm_pubkey_hex.clone(),
            new_psm_endpoint: proposal.metadata.new_psm_endpoint.clone(),
        };

        Self {
            version: EXPORT_VERSION,
            account_id: account_id.to_string(),
            id: proposal.id.clone(),
            nonce: proposal.nonce,
            transaction_type: tx_type_str.to_string(),
            tx_summary: proposal
                .metadata
                .tx_summary_json
                .clone()
                .unwrap_or_else(|| serde_json::json!({})),
            signatures,
            signatures_required,
            metadata,
        }
    }

    /// Creates an ExportedProposal with signatures from raw data.
    pub fn with_signatures(mut self, signatures: Vec<ExportedSignature>) -> Self {
        self.signatures = signatures;
        self
    }

    /// Converts the ExportedProposal back to a Proposal.
    pub fn to_proposal(&self) -> Result<Proposal> {
        // Parse transaction summary
        let tx_summary = TransactionSummary::from_json(&self.tx_summary).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;

        // Parse account ID
        let _account_id = AccountId::from_hex(&self.account_id)
            .map_err(|e| MultisigError::InvalidConfig(format!("invalid account_id: {}", e)))?;

        // Build ProposalMetadata
        let metadata = ProposalMetadata {
            tx_summary_json: Some(self.tx_summary.clone()),
            new_threshold: self.metadata.new_threshold,
            signer_commitments_hex: self.metadata.signer_commitments_hex.clone(),
            salt_hex: self.metadata.salt_hex.clone(),
            recipient_hex: self.metadata.recipient_hex.clone(),
            faucet_id_hex: self.metadata.faucet_id_hex.clone(),
            amount: self.metadata.amount,
            note_ids_hex: self.metadata.note_ids_hex.clone(),
            new_psm_pubkey_hex: self.metadata.new_psm_pubkey_hex.clone(),
            new_psm_endpoint: self.metadata.new_psm_endpoint.clone(),
            required_signatures: Some(self.signatures_required),
            collected_signatures: Some(self.signatures.len()),
        };

        // Determine transaction type from the string
        let transaction_type = self.parse_transaction_type(&metadata)?;

        // Build status
        let signers: Vec<String> = self
            .signatures
            .iter()
            .map(|s| s.signer_commitment.clone())
            .collect();

        let status = if self.signatures.len() >= self.signatures_required {
            ProposalStatus::Ready
        } else {
            ProposalStatus::Pending {
                signatures_collected: self.signatures.len(),
                signatures_required: self.signatures_required,
                signers,
            }
        };

        Ok(Proposal {
            id: self.id.clone(),
            nonce: self.nonce,
            transaction_type,
            status,
            tx_summary,
            metadata,
        })
    }

    /// Parses the transaction type from the string representation.
    fn parse_transaction_type(&self, metadata: &ProposalMetadata) -> Result<TransactionType> {
        match self.transaction_type.as_str() {
            "P2ID" => {
                let recipient_hex = metadata
                    .recipient_hex
                    .as_ref()
                    .ok_or_else(|| MultisigError::MissingConfig("recipient_hex".to_string()))?;
                let faucet_id_hex = metadata
                    .faucet_id_hex
                    .as_ref()
                    .ok_or_else(|| MultisigError::MissingConfig("faucet_id_hex".to_string()))?;
                let amount = metadata
                    .amount
                    .ok_or_else(|| MultisigError::MissingConfig("amount".to_string()))?;

                let recipient = AccountId::from_hex(recipient_hex).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid recipient: {}", e))
                })?;
                let faucet_id = AccountId::from_hex(faucet_id_hex).map_err(|e| {
                    MultisigError::InvalidConfig(format!("invalid faucet_id: {}", e))
                })?;

                Ok(TransactionType::P2ID {
                    recipient,
                    faucet_id,
                    amount,
                })
            }
            "ConsumeNotes" => {
                let note_ids = metadata.note_ids()?;
                Ok(TransactionType::ConsumeNotes { note_ids })
            }
            "AddCosigner" => {
                // Find the new commitment (last one in the list that's being added)
                let commitments = metadata.signer_commitments()?;
                let new_commitment = commitments.last().cloned().ok_or_else(|| {
                    MultisigError::MissingConfig("new cosigner commitment".to_string())
                })?;
                Ok(TransactionType::AddCosigner { new_commitment })
            }
            "RemoveCosigner" => {
                // For remove, we'd need to track which was removed
                // For now, return UpdateSigners as a fallback
                let signer_commitments = metadata.signer_commitments()?;
                let new_threshold = metadata
                    .new_threshold
                    .ok_or_else(|| MultisigError::MissingConfig("new_threshold".to_string()))?
                    as u32;
                Ok(TransactionType::UpdateSigners {
                    new_threshold,
                    signer_commitments,
                })
            }
            "SwitchPsm" => {
                let pubkey_hex = metadata.new_psm_pubkey_hex.as_ref().ok_or_else(|| {
                    MultisigError::MissingConfig("new_psm_pubkey_hex".to_string())
                })?;
                let endpoint = metadata
                    .new_psm_endpoint
                    .as_ref()
                    .ok_or_else(|| MultisigError::MissingConfig("new_psm_endpoint".to_string()))?;

                let new_commitment = hex_to_word(pubkey_hex)?;
                Ok(TransactionType::SwitchPsm {
                    new_endpoint: endpoint.clone(),
                    new_commitment,
                })
            }
            "UpdateSigners" => {
                let signer_commitments = metadata.signer_commitments()?;
                let new_threshold = metadata
                    .new_threshold
                    .ok_or_else(|| MultisigError::MissingConfig("new_threshold".to_string()))?
                    as u32;
                Ok(TransactionType::UpdateSigners {
                    new_threshold,
                    signer_commitments,
                })
            }
            _ => Ok(TransactionType::Unknown),
        }
    }

    /// Returns the number of signatures collected.
    pub fn signatures_collected(&self) -> usize {
        self.signatures.len()
    }

    /// Returns true if the proposal has enough signatures for execution.
    pub fn is_ready(&self) -> bool {
        self.signatures.len() >= self.signatures_required
    }

    /// Adds a signature to the proposal.
    ///
    /// Returns an error if the signer has already signed.
    pub fn add_signature(&mut self, signature: ExportedSignature) -> Result<()> {
        // Check if already signed
        if self.signatures.iter().any(|s| {
            s.signer_commitment
                .eq_ignore_ascii_case(&signature.signer_commitment)
        }) {
            return Err(MultisigError::AlreadySigned);
        }

        self.signatures.push(signature);
        Ok(())
    }

    /// Returns the account ID as an AccountId.
    pub fn account_id(&self) -> Result<AccountId> {
        AccountId::from_hex(&self.account_id)
            .map_err(|e| MultisigError::InvalidConfig(format!("invalid account_id: {}", e)))
    }

    /// Serializes the proposal to a JSON string.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(self).map_err(MultisigError::Serialization)
    }

    /// Deserializes a proposal from a JSON string.
    pub fn from_json(json: &str) -> Result<Self> {
        let exported: Self = serde_json::from_str(json)?;

        // Validate version
        if exported.version > EXPORT_VERSION {
            return Err(MultisigError::InvalidConfig(format!(
                "unsupported export version {}, maximum supported is {}",
                exported.version, EXPORT_VERSION
            )));
        }

        Ok(exported)
    }
}

/// Converts a hex string to Word.
fn hex_to_word(hex: &str) -> Result<miden_objects::Word> {
    use miden_objects::Felt;

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
    Ok(miden_objects::Word::from(word.map(Felt::new)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exported_signature_serialization() {
        let sig = ExportedSignature {
            signer_commitment: "0xabc123".to_string(),
            signature: "0xdef456".to_string(),
        };

        let json = serde_json::to_string(&sig).expect("should serialize");
        let parsed: ExportedSignature = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(sig.signer_commitment, parsed.signer_commitment);
        assert_eq!(sig.signature, parsed.signature);
    }

    #[test]
    fn test_exported_metadata_serialization() {
        let meta = ExportedMetadata {
            salt_hex: Some("0x123".to_string()),
            new_threshold: Some(2),
            signer_commitments_hex: vec!["0xabc".to_string()],
            recipient_hex: None,
            faucet_id_hex: None,
            amount: None,
            note_ids_hex: vec![],
            new_psm_pubkey_hex: None,
            new_psm_endpoint: None,
        };

        let json = serde_json::to_string(&meta).expect("should serialize");
        let parsed: ExportedMetadata = serde_json::from_str(&json).expect("should deserialize");

        assert_eq!(meta.salt_hex, parsed.salt_hex);
        assert_eq!(meta.new_threshold, parsed.new_threshold);
    }

    #[test]
    fn test_add_signature_prevents_duplicates() {
        let mut proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: "0x123".to_string(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "UpdateSigners".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata::default(),
        };

        let sig1 = ExportedSignature {
            signer_commitment: "0xsigner1".to_string(),
            signature: "0xsig1".to_string(),
        };

        // First signature should succeed
        proposal.add_signature(sig1.clone()).expect("should add");
        assert_eq!(proposal.signatures.len(), 1);

        // Duplicate should fail
        let result = proposal.add_signature(sig1);
        assert!(result.is_err());
        assert_eq!(proposal.signatures.len(), 1);
    }

    #[test]
    fn test_is_ready() {
        let mut proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: "0x123".to_string(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "UpdateSigners".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata::default(),
        };

        assert!(!proposal.is_ready());

        proposal.signatures.push(ExportedSignature {
            signer_commitment: "0xsigner1".to_string(),
            signature: "0xsig1".to_string(),
        });
        assert!(!proposal.is_ready());

        proposal.signatures.push(ExportedSignature {
            signer_commitment: "0xsigner2".to_string(),
            signature: "0xsig2".to_string(),
        });
        assert!(proposal.is_ready());
    }

    #[test]
    fn test_version_validation() {
        let json = r#"{
            "version": 999,
            "account_id": "0x123",
            "id": "0xabc",
            "nonce": 1,
            "transaction_type": "UpdateSigners",
            "tx_summary": {},
            "signatures": [],
            "signatures_required": 2,
            "metadata": {}
        }"#;

        let result = ExportedProposal::from_json(json);
        assert!(result.is_err());
    }
}
