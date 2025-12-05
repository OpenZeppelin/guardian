//! Payload types for multisig transaction proposals.

use miden_objects::transaction::TransactionSummary;
use private_state_manager_shared::{DeltaSignature, ProposalSignature, ToJson};
use serde::{Deserialize, Serialize};

use crate::keystore::KeyManager;

/// Metadata for multisig transaction proposals.
///
/// This contains information needed to reconstruct and execute the transaction
/// after all signatures have been collected.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ProposalMetadataPayload {
    /// New threshold after the transaction (for signer updates).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_threshold: Option<u64>,
    /// Signer commitments as hex strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_commitments_hex: Vec<String>,
    /// Salt used for transaction authentication.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub salt_hex: Option<String>,

    // Payment (P2ID) fields
    /// Recipient account ID as hex string (for P2ID transfers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_hex: Option<String>,
    /// Faucet ID as hex string (for P2ID transfers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faucet_id_hex: Option<String>,
    /// Amount to transfer (for P2ID transfers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<u64>,

    // Note consumption fields
    /// Note IDs to consume as hex strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note_ids_hex: Vec<String>,

    // PSM update fields
    /// New PSM public key commitment as hex string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_psm_pubkey_hex: Option<String>,
    /// New PSM endpoint URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_psm_endpoint: Option<String>,
}

/// Complete payload for a multisig transaction proposal.
///
/// This is the structured format sent to PSM when creating a proposal.
/// It contains:
/// - The transaction summary (serialized)
/// - Initial signatures from the proposer
/// - Metadata needed for execution
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProposalPayload {
    /// The transaction summary.
    pub tx_summary: serde_json::Value,
    /// Signatures collected so far.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<DeltaSignature>,
    /// Metadata for the proposal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ProposalMetadataPayload>,
}

impl ProposalPayload {
    /// Creates a new proposal payload from a transaction summary.
    pub fn new(tx_summary: &TransactionSummary) -> Self {
        Self {
            tx_summary: tx_summary.to_json(),
            signatures: Vec::new(),
            metadata: None,
        }
    }

    /// Adds the proposer's signature.
    pub fn with_signature(
        mut self,
        key_manager: &dyn KeyManager,
        message: miden_objects::Word,
    ) -> Self {
        let signature_hex = key_manager.sign_hex(message);
        self.signatures.push(DeltaSignature {
            signer_id: key_manager.commitment_hex(),
            signature: ProposalSignature::Falcon {
                signature: signature_hex,
            },
        });
        self
    }

    /// Sets the metadata for signer updates.
    pub fn with_signer_metadata(
        mut self,
        new_threshold: u64,
        signer_commitments_hex: Vec<String>,
        salt_hex: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            new_threshold: Some(new_threshold),
            signer_commitments_hex,
            salt_hex: Some(salt_hex),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for P2ID payment transfers.
    pub fn with_payment_metadata(
        mut self,
        recipient_hex: String,
        faucet_id_hex: String,
        amount: u64,
        salt_hex: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            recipient_hex: Some(recipient_hex),
            faucet_id_hex: Some(faucet_id_hex),
            amount: Some(amount),
            salt_hex: Some(salt_hex),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for note consumption transactions.
    pub fn with_note_consumption_metadata(
        mut self,
        note_ids_hex: &[String],
        salt_hex: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            note_ids_hex: note_ids_hex.to_vec(),
            salt_hex: Some(salt_hex),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for PSM update transactions.
    pub fn with_psm_update_metadata(
        mut self,
        new_psm_pubkey_hex: String,
        new_psm_endpoint: String,
        salt_hex: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            new_psm_pubkey_hex: Some(new_psm_pubkey_hex),
            new_psm_endpoint: Some(new_psm_endpoint),
            salt_hex: Some(salt_hex),
            ..Default::default()
        });
        self
    }

    /// Converts to JSON value for sending to PSM.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("ProposalPayload should always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proposal_payload_serialization() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({"data": "test"}),
            signatures: vec![DeltaSignature {
                signer_id: "0xabc".to_string(),
                signature: ProposalSignature::Falcon {
                    signature: "0x123".to_string(),
                },
            }],
            metadata: Some(ProposalMetadataPayload {
                new_threshold: Some(2),
                signer_commitments_hex: vec!["0xabc".to_string(), "0xdef".to_string()],
                salt_hex: Some("0x456".to_string()),
                ..Default::default()
            }),
        };

        let json = payload.to_json();

        assert!(json.get("tx_summary").is_some());
        assert!(json.get("signatures").is_some());
        assert!(json.get("metadata").is_some());

        let metadata = json.get("metadata").unwrap();
        assert_eq!(metadata.get("new_threshold").unwrap().as_u64(), Some(2));
    }
}
