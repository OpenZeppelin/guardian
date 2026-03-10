//! Export/import types for offline proposal sharing.
//!
//! This module provides types and utilities for exporting proposals to files
//! and importing them back. This enables offline sharing of proposals via
//! side channels (email, USB, etc.) when the PSM server is unavailable.
//!

use std::collections::HashSet;

use miden_protocol::account::AccountId;
use miden_protocol::crypto::dsa::falcon512_rpo::Signature as RpoFalconSignature;
use miden_protocol::transaction::TransactionSummary;
use miden_protocol::Word;
use private_state_manager_shared::FromJson;
use private_state_manager_shared::hex::FromHex;
use serde::{Deserialize, Serialize};

use crate::error::{MultisigError, Result};
use crate::keystore::ensure_hex_prefix;
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus};
use crate::utils::hex_body_eq;

/// Current export format version.
pub const EXPORT_VERSION: u32 = 1;

/// Exported proposal for offline sharing.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExportedProposal {
    pub version: u32,
    pub account_id: String,

    pub id: String,
    pub nonce: u64,

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
    #[serde(default)]
    pub proposal_type: String,

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
    fn metadata(&self) -> ProposalMetadata {
        ProposalMetadata {
            tx_summary_json: Some(self.tx_summary.clone()),
            proposal_type: Some(self.metadata.proposal_type.clone()),
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
            signers: self
                .signatures
                .iter()
                .map(|signature| signature.signer_commitment.clone())
                .collect(),
        }
    }

    fn expected_id(tx_summary: &TransactionSummary) -> String {
        format!(
            "0x{}",
            hex::encode(miden_protocol::utils::serde::Serializable::to_bytes(
                &tx_summary.to_commitment(),
            ))
        )
    }

    fn validate_signatures(&self) -> Result<()> {
        let mut seen_signers = HashSet::new();

        for signature in &self.signatures {
            Word::from_hex(&signature.signer_commitment).map_err(MultisigError::InvalidConfig)?;

            let signature_hex = ensure_hex_prefix(&signature.signature);
            RpoFalconSignature::from_hex(&signature_hex).map_err(|e| {
                MultisigError::Signature(format!("invalid exported signature: {}", e))
            })?;

            if !seen_signers.insert(signature.signer_commitment.to_lowercase()) {
                return Err(MultisigError::InvalidConfig(format!(
                    "duplicate exported signature for signer {}",
                    signature.signer_commitment
                )));
            }
        }

        Ok(())
    }

    pub fn validate(&self, expected_account_id: Option<AccountId>) -> Result<()> {
        let account_id = self.account_id()?;
        if let Some(expected_account_id) = expected_account_id
            && account_id != expected_account_id
        {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal account {} does not match loaded account {}",
                self.account_id, expected_account_id
            )));
        }

        if self.id.is_empty() {
            return Err(MultisigError::InvalidConfig(
                "proposal id is required".to_string(),
            ));
        }

        if self.signatures_required == 0 {
            return Err(MultisigError::InvalidConfig(
                "signatures_required must be greater than 0".to_string(),
            ));
        }

        let tx_summary = TransactionSummary::from_json(&self.tx_summary).map_err(|e| {
            MultisigError::InvalidConfig(format!("failed to parse tx_summary: {}", e))
        })?;
        let expected_id = Self::expected_id(&tx_summary);
        if !hex_body_eq(&self.id, &expected_id) {
            return Err(MultisigError::InvalidConfig(format!(
                "proposal id {} does not match tx_summary commitment {}",
                self.id, expected_id
            )));
        }

        let metadata = self.metadata();
        metadata.to_transaction_type(&self.metadata.proposal_type)?;
        self.validate_signatures()
    }

    /// Creates an ExportedProposal from a Proposal and account ID.
    pub fn from_proposal(proposal: &Proposal, account_id: AccountId) -> Result<Self> {
        let proposal_type = proposal
            .metadata
            .proposal_type
            .clone()
            .or_else(|| {
                proposal
                    .transaction_type
                    .proposal_type()
                    .map(str::to_string)
            })
            .ok_or_else(|| {
                MultisigError::InvalidConfig(
                    "cannot export signer update proposal without metadata.proposal_type"
                        .to_string(),
                )
            })?;
        let signatures_required = proposal.signatures_required();

        let signatures = Vec::new();

        let metadata = ExportedMetadata {
            proposal_type,
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

        Ok(Self {
            version: EXPORT_VERSION,
            account_id: account_id.to_string(),
            id: proposal.id.clone(),
            nonce: proposal.nonce,
            tx_summary: proposal
                .metadata
                .tx_summary_json
                .clone()
                .unwrap_or_else(|| serde_json::json!({})),
            signatures,
            signatures_required,
            metadata,
        })
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
        validate_proposal_id_matches_summary(&self.id, &tx_summary)?;

        AccountId::from_hex(&self.account_id)
            .map_err(|e| MultisigError::InvalidConfig(format!("invalid account_id: {}", e)))?;

        let metadata = self.metadata();
        let transaction_type = metadata.to_transaction_type(&self.metadata.proposal_type)?;

        let status = if self.signatures.len() >= self.signatures_required {
            ProposalStatus::Ready
        } else {
            ProposalStatus::Pending
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
        Word::from_hex(&signature.signer_commitment).map_err(MultisigError::InvalidConfig)?;

        let signature_hex = ensure_hex_prefix(&signature.signature);
        RpoFalconSignature::from_hex(&signature_hex)
            .map_err(|e| MultisigError::Signature(format!("invalid exported signature: {}", e)))?;

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

        exported.validate(None)?;

        Ok(exported)
    }
}

fn validate_proposal_id_matches_summary(id: &str, tx_summary: &TransactionSummary) -> Result<()> {
    let expected_id = ExportedProposal::expected_id(tx_summary);
    if !hex_body_eq(id, &expected_id) {
        return Err(MultisigError::InvalidConfig(format!(
            "proposal id {} does not match tx_summary commitment {}",
            id, expected_id
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use miden_client::Serializable;
    use miden_protocol::FieldElement;
    use miden_protocol::account::AccountId;
    use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
    use miden_protocol::crypto::dsa::falcon512_rpo::SecretKey;
    use miden_protocol::transaction::{InputNotes, OutputNotes, TransactionSummary};
    use miden_protocol::{Felt, Word, ZERO};
    use private_state_manager_shared::ToJson;

    use crate::proposal::TransactionType;
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
            proposal_type: "add_signer".to_string(),
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
        assert_eq!(meta.proposal_type, parsed.proposal_type);
    }

    #[test]
    fn test_add_signature_prevents_duplicates() {
        let mut proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: "0x123".to_string(),
            id: "0xabc".to_string(),
            nonce: 1,
            tx_summary: create_test_tx_summary().to_json(),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata {
                proposal_type: "change_threshold".to_string(),
                new_threshold: Some(2),
                signer_commitments_hex: vec![valid_word_hex()],
                ..Default::default()
            },
        };

        let sig1 = valid_exported_signature();

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
    fn test_version_validation_rejects_future_exports() {
        let json = r#"{
            "version": 999,
            "account_id": "0x123",
            "id": "0xabc",
            "nonce": 1,
            "tx_summary": {},
            "signatures": [],
            "signatures_required": 2,
            "metadata": {
                "proposal_type": "change_threshold"
            }
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

    fn create_test_tx_summary() -> TransactionSummary {
        let account_id = AccountId::from_hex(&valid_account_id()).expect("valid account id");
        let account_delta = AccountDelta::new(
            account_id,
            AccountStorageDelta::default(),
            AccountVaultDelta::default(),
            Felt::ZERO,
        )
        .expect("valid delta");

        TransactionSummary::new(
            account_delta,
            InputNotes::new(Vec::new()).expect("empty input notes"),
            OutputNotes::new(Vec::new()).expect("empty output notes"),
            Word::from([Felt::new(7), ZERO, ZERO, ZERO]),
        )
    }

    fn valid_proposal_id() -> String {
        ExportedProposal::expected_id(&create_test_tx_summary())
    }

    fn valid_exported_signature() -> ExportedSignature {
        let secret_key = SecretKey::new();
        let signature = secret_key.sign(create_test_tx_summary().to_commitment());
        ExportedSignature {
            signer_commitment: format!(
                "0x{}",
                hex::encode(secret_key.public_key().to_commitment().to_bytes())
            ),
            signature: format!("0x{}", hex::encode(signature.to_bytes())),
        }
    }

    #[test]
    fn validate_rejects_commitment_mismatch() {
        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: valid_word_hex(),
            nonce: 1,
            tx_summary: create_test_tx_summary().to_json(),
            signatures: vec![],
            signatures_required: 1,
            metadata: ExportedMetadata {
                proposal_type: "consume_notes".to_string(),
                note_ids_hex: vec![valid_note_id_hex()],
                ..Default::default()
            },
        };

        let result = proposal.validate(None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("does not match tx_summary commitment")
        );
    }

    #[test]
    fn validate_rejects_duplicate_signatures() {
        let signature = valid_exported_signature();
        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: valid_proposal_id(),
            nonce: 1,
            tx_summary: create_test_tx_summary().to_json(),
            signatures: vec![signature.clone(), signature],
            signatures_required: 2,
            metadata: ExportedMetadata {
                proposal_type: "change_threshold".to_string(),
                new_threshold: Some(2),
                signer_commitments_hex: vec![valid_word_hex()],
                ..Default::default()
            },
        };

        let result = proposal.validate(None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("duplicate exported signature")
        );
    }

    #[test]
    fn validate_rejects_invalid_signature_hex() {
        let mut signature = valid_exported_signature();
        signature.signature = "0x1234".to_string();

        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: valid_proposal_id(),
            nonce: 1,
            tx_summary: create_test_tx_summary().to_json(),
            signatures: vec![signature],
            signatures_required: 2,
            metadata: ExportedMetadata {
                proposal_type: "change_threshold".to_string(),
                new_threshold: Some(2),
                signer_commitments_hex: vec![valid_word_hex()],
                ..Default::default()
            },
        };

        let result = proposal.validate(None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("invalid exported signature")
        );
    }
    #[test]
    fn to_proposal_uses_metadata_proposal_type_for_p2id() {
        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: valid_proposal_id(),
            nonce: 1,
            tx_summary: create_test_tx_summary().to_json(),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata {
                proposal_type: "p2id".to_string(),
                recipient_hex: Some(valid_account_id()),
                faucet_id_hex: Some(valid_faucet_id()),
                amount: Some(1000),
                ..Default::default()
            },
        };

        let parsed = proposal.to_proposal().expect("proposal should parse");

        assert!(matches!(
            parsed.transaction_type,
            TransactionType::P2ID { amount: 1000, .. }
        ));
    }

    #[test]
    fn to_proposal_uses_metadata_proposal_type_for_update_signers() {
        let proposal = ExportedProposal {
            version: EXPORT_VERSION,
            account_id: valid_account_id(),
            id: valid_proposal_id(),
            nonce: 1,
            tx_summary: create_test_tx_summary().to_json(),
            signatures: vec![],
            signatures_required: 2,
            metadata: ExportedMetadata {
                proposal_type: "add_signer".to_string(),
                new_threshold: Some(2),
                signer_commitments_hex: vec![valid_word_hex()],
                ..Default::default()
            },
        };

        let parsed = proposal.to_proposal().expect("proposal should parse");

        assert!(matches!(
            parsed.transaction_type,
            TransactionType::UpdateSigners {
                new_threshold: 2,
                ..
            }
        ));
        assert_eq!(parsed.metadata.proposal_type.as_deref(), Some("add_signer"));
    }

    #[test]
    fn to_proposal_rejects_missing_proposal_type() {
        let json = format!(
            r#"{{
                "version": {version},
                "account_id": "{account_id}",
                "id": "{id}",
                "nonce": 1,
                "tx_summary": {tx_summary},
                "signatures": [],
                "signatures_required": 2,
                "metadata": {{
                    "new_threshold": 2,
                    "signer_commitments_hex": ["{commitment}"]
                }}
            }}"#,
            version = EXPORT_VERSION,
            account_id = valid_account_id(),
            id = valid_proposal_id(),
            tx_summary = create_test_tx_summary().to_json(),
            commitment = valid_word_hex(),
        );

        let result = ExportedProposal::from_json(&json);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("proposal metadata.proposal_type is required")
        );
    }

    #[test]
    fn from_proposal_roundtrip_preserves_proposal_type() {
        let new_commitment = Word::from_hex(&valid_word_hex()).expect("valid signer commitment");
        let tx_summary = create_test_tx_summary();
        let proposal = Proposal::new(
            tx_summary.clone(),
            1,
            TransactionType::AddCosigner { new_commitment },
            ProposalMetadata {
                tx_summary_json: Some(tx_summary.to_json()),
                new_threshold: Some(2),
                signer_commitments_hex: vec![valid_word_hex()],
                required_signatures: Some(2),
                ..Default::default()
            },
        );

        assert_eq!(
            proposal.metadata.proposal_type.as_deref(),
            Some("add_signer")
        );

        let account_id = AccountId::from_hex(&valid_account_id()).expect("valid account id");
        let exported =
            ExportedProposal::from_proposal(&proposal, account_id).expect("proposal should export");

        assert_eq!(exported.metadata.proposal_type, "add_signer");

        let imported = exported.to_proposal().expect("proposal should parse");
        assert!(matches!(
            imported.transaction_type,
            TransactionType::UpdateSigners {
                new_threshold: 2,
                ..
            }
        ));
        assert_eq!(
            imported.metadata.proposal_type.as_deref(),
            Some("add_signer")
        );
    }

    #[test]
    fn from_proposal_rejects_ambiguous_update_signers_without_proposal_type() {
        let tx_summary = create_test_tx_summary();
        let proposal = Proposal::new(
            tx_summary.clone(),
            1,
            TransactionType::UpdateSigners {
                new_threshold: 2,
                signer_commitments: vec![Word::from_hex(&valid_word_hex()).expect("valid word")],
            },
            ProposalMetadata {
                tx_summary_json: Some(tx_summary.to_json()),
                new_threshold: Some(2),
                signer_commitments_hex: vec![valid_word_hex()],
                required_signatures: Some(2),
                ..Default::default()
            },
        );

        let account_id = AccountId::from_hex(&valid_account_id()).expect("valid account id");
        let result = ExportedProposal::from_proposal(&proposal, account_id);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cannot export signer update proposal without metadata.proposal_type")
        );
    }
}
