//! Export/import types for offline proposal sharing.
//!
//! This module provides types and utilities for exporting proposals to files
//! and importing them back. This enables offline sharing of proposals via
//! side channels (email, USB, etc.) when the PSM server is unavailable.
//!

use miden_objects::account::AccountId;
use miden_objects::transaction::TransactionSummary;
use private_state_manager_shared::FromJson;
use serde::{Deserialize, Serialize};

use crate::error::{MultisigError, Result};
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

/// Current export format version.
pub const EXPORT_VERSION: u32 = 1;

/// Exported proposal for offline sharing.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExportedProposal {
    pub version: u32,
    pub account_id: String,

    pub id: String,
    pub nonce: u64,

    pub transaction_type: String,
    pub tx_summary: serde_json::Value,

    #[serde(default)]
    pub signatures: Vec<ExportedSignature>,

    pub signatures_required: usize,
    pub metadata: ExportedMetadata,
}

/// A signature collected for an exported proposal.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExportedSignature {
    pub signer_commitment: String,
    pub signature: String,
}

/// Metadata needed for proposal reconstruction.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ExportedMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salt_hex: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_threshold: Option<u64>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_commitments_hex: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_hex: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub faucet_id_hex: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note_ids_hex: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_psm_pubkey_hex: Option<String>,

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
        };

        let signatures_required = proposal.signatures_required();

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
        let tx_summary = TransactionSummary::from_json(&self.tx_summary).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;

        let _account_id = AccountId::from_hex(&self.account_id)
            .map_err(|e| MultisigError::InvalidConfig(format!("invalid account_id: {}", e)))?;

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

        let transaction_type = self.parse_transaction_type(&metadata)?;

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
                let commitments = metadata.signer_commitments()?;
                let new_commitment = commitments.last().cloned().ok_or_else(|| {
                    MultisigError::MissingConfig("new cosigner commitment".to_string())
                })?;
                Ok(TransactionType::AddCosigner { new_commitment })
            }
            "RemoveCosigner" => {
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
            other => Err(MultisigError::UnknownTransactionType(other.to_string())),
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

    /// Returns (collected, required) signature counts.
    pub fn signature_counts(&self) -> (usize, usize) {
        (self.signatures.len(), self.signatures_required)
    }

    /// Returns the number of additional signatures needed for finalization.
    /// Returns 0 if the proposal is ready.
    pub fn signatures_needed(&self) -> usize {
        self.signatures_required
            .saturating_sub(self.signatures.len())
    }

    /// Checks if a signer (by commitment hex) has already signed this proposal.
    pub fn has_signed(&self, commitment_hex: &str) -> bool {
        self.signatures
            .iter()
            .any(|s| s.signer_commitment.eq_ignore_ascii_case(commitment_hex))
    }

    /// Returns the commitment hex strings of all signers who have signed.
    pub fn signed_by(&self) -> Vec<&str> {
        self.signatures
            .iter()
            .map(|s| s.signer_commitment.as_str())
            .collect()
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

    #[test]
    fn test_signature_counts() {
        let mut proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: "0x123".to_string(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "UpdateSigners".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 3,
            metadata: ExportedMetadata::default(),
        };

        assert_eq!(proposal.signature_counts(), (0, 3));
        assert_eq!(proposal.signatures_needed(), 3);

        proposal.signatures.push(ExportedSignature {
            signer_commitment: "0xsigner1".to_string(),
            signature: "0xsig1".to_string(),
        });

        assert_eq!(proposal.signature_counts(), (1, 3));
        assert_eq!(proposal.signatures_needed(), 2);
    }

    #[test]
    fn test_has_signed() {
        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: "0x123".to_string(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "UpdateSigners".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![
                ExportedSignature {
                    signer_commitment: "0xSigner1".to_string(),
                    signature: "0xsig1".to_string(),
                },
                ExportedSignature {
                    signer_commitment: "0xsigner2".to_string(),
                    signature: "0xsig2".to_string(),
                },
            ],
            signatures_required: 3,
            metadata: ExportedMetadata::default(),
        };

        // Test case-insensitive matching
        assert!(proposal.has_signed("0xsigner1"));
        assert!(proposal.has_signed("0xSIGNER1"));
        assert!(proposal.has_signed("0xSigner2"));
        assert!(!proposal.has_signed("0xsigner3"));
    }

    #[test]
    fn test_signed_by() {
        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: "0x123".to_string(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "UpdateSigners".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![
                ExportedSignature {
                    signer_commitment: "0xsigner1".to_string(),
                    signature: "0xsig1".to_string(),
                },
                ExportedSignature {
                    signer_commitment: "0xsigner2".to_string(),
                    signature: "0xsig2".to_string(),
                },
            ],
            signatures_required: 3,
            metadata: ExportedMetadata::default(),
        };

        let signers = proposal.signed_by();
        assert_eq!(signers.len(), 2);
        assert!(signers.contains(&"0xsigner1"));
        assert!(signers.contains(&"0xsigner2"));
    }

    // Helper for valid account ID (15 bytes = 30 hex chars)
    fn valid_account_id() -> String {
        "0x7bfb0f38b0fafa103f86a805594170".to_string()
    }

    fn valid_faucet_id() -> String {
        "0x7bfb0f38b0fafa103f86a805594171".to_string()
    }

    // Helper for valid 32-byte hex (Word)
    fn valid_word_hex() -> String {
        "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20".to_string()
    }

    // Helper for valid note ID hex (32 bytes)
    fn valid_note_id_hex() -> String {
        "0x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20".to_string()
    }

    #[test]
    fn test_parse_transaction_type_p2id() {
        let metadata = ProposalMetadata {
            recipient_hex: Some(valid_account_id()),
            faucet_id_hex: Some(valid_faucet_id()),
            amount: Some(1000),
            ..Default::default()
        };

        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "P2ID".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata {
                recipient_hex: metadata.recipient_hex.clone(),
                faucet_id_hex: metadata.faucet_id_hex.clone(),
                amount: metadata.amount,
                ..Default::default()
            },
        };

        let result = proposal.parse_transaction_type(&metadata);
        assert!(result.is_ok(), "expected Ok, got: {:?}", result);
        let tx_type = result.unwrap();
        assert!(matches!(
            tx_type,
            TransactionType::P2ID { amount: 1000, .. }
        ));
    }

    #[test]
    fn test_parse_transaction_type_consume_notes() {
        let metadata = ProposalMetadata {
            note_ids_hex: vec![valid_note_id_hex()],
            ..Default::default()
        };

        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "ConsumeNotes".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata {
                note_ids_hex: metadata.note_ids_hex.clone(),
                ..Default::default()
            },
        };

        let result = proposal.parse_transaction_type(&metadata);
        assert!(result.is_ok());
        let tx_type = result.unwrap();
        assert!(matches!(tx_type, TransactionType::ConsumeNotes { .. }));
    }

    #[test]
    fn test_parse_transaction_type_add_cosigner() {
        let metadata = ProposalMetadata {
            signer_commitments_hex: vec![valid_word_hex()],
            ..Default::default()
        };

        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "AddCosigner".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata {
                signer_commitments_hex: metadata.signer_commitments_hex.clone(),
                ..Default::default()
            },
        };

        let result = proposal.parse_transaction_type(&metadata);
        assert!(result.is_ok());
        let tx_type = result.unwrap();
        assert!(matches!(tx_type, TransactionType::AddCosigner { .. }));
    }

    #[test]
    fn test_parse_transaction_type_switch_psm() {
        let metadata = ProposalMetadata {
            new_psm_pubkey_hex: Some(valid_word_hex()),
            new_psm_endpoint: Some("http://new-psm:50051".to_string()),
            ..Default::default()
        };

        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "SwitchPsm".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata {
                new_psm_pubkey_hex: metadata.new_psm_pubkey_hex.clone(),
                new_psm_endpoint: metadata.new_psm_endpoint.clone(),
                ..Default::default()
            },
        };

        let result = proposal.parse_transaction_type(&metadata);
        assert!(result.is_ok());
        let tx_type = result.unwrap();
        match tx_type {
            TransactionType::SwitchPsm { new_endpoint, .. } => {
                assert_eq!(new_endpoint, "http://new-psm:50051");
            }
            _ => panic!("expected SwitchPsm"),
        }
    }

    #[test]
    fn test_parse_transaction_type_invalid() {
        let metadata = ProposalMetadata::default();

        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: "0xabc".to_string(),
            nonce: 1,
            transaction_type: "InvalidType".to_string(),
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata::default(),
        };

        let result = proposal.parse_transaction_type(&metadata);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, MultisigError::UnknownTransactionType(_)));
    }

    #[test]
    fn test_hex_to_word_valid() {
        let hex = valid_word_hex();
        let result = hex_to_word(&hex);
        assert!(result.is_ok());
    }

    #[test]
    fn test_hex_to_word_invalid_length() {
        let hex = "0x1234"; // Too short
        let result = hex_to_word(hex);
        assert!(result.is_err());
    }

    #[test]
    fn test_hex_to_word_invalid_chars() {
        let hex = "0xGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG";
        let result = hex_to_word(hex);
        assert!(result.is_err());
    }
}
