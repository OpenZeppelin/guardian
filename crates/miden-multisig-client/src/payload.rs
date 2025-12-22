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
    fn proposal_payload_serialization_includes_all_fields() {
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

    #[test]
    fn with_signer_metadata_sets_fields() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        }
        .with_signer_metadata(
            3,
            vec!["0xabc".to_string(), "0xdef".to_string()],
            "0xsalt".to_string(),
        );

        let meta = payload.metadata.unwrap();
        assert_eq!(meta.new_threshold, Some(3));
        assert_eq!(meta.signer_commitments_hex.len(), 2);
        assert_eq!(meta.salt_hex, Some("0xsalt".to_string()));
    }

    #[test]
    fn with_payment_metadata_sets_fields() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        }
        .with_payment_metadata(
            "0xrecipient".to_string(),
            "0xfaucet".to_string(),
            1000,
            "0xsalt".to_string(),
        );

        let meta = payload.metadata.unwrap();
        assert_eq!(meta.recipient_hex, Some("0xrecipient".to_string()));
        assert_eq!(meta.faucet_id_hex, Some("0xfaucet".to_string()));
        assert_eq!(meta.amount, Some(1000));
        assert_eq!(meta.salt_hex, Some("0xsalt".to_string()));
    }

    #[test]
    fn with_note_consumption_metadata_sets_fields() {
        let note_ids = vec!["0xnote1".to_string(), "0xnote2".to_string()];
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        }
        .with_note_consumption_metadata(&note_ids, "0xsalt".to_string());

        let meta = payload.metadata.unwrap();
        assert_eq!(meta.note_ids_hex.len(), 2);
        assert_eq!(meta.note_ids_hex[0], "0xnote1");
        assert_eq!(meta.salt_hex, Some("0xsalt".to_string()));
    }

    #[test]
    fn with_psm_update_metadata_sets_fields() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        }
        .with_psm_update_metadata(
            "0xpubkey".to_string(),
            "http://new-psm:50051".to_string(),
            "0xsalt".to_string(),
        );

        let meta = payload.metadata.unwrap();
        assert_eq!(meta.new_psm_pubkey_hex, Some("0xpubkey".to_string()));
        assert_eq!(
            meta.new_psm_endpoint,
            Some("http://new-psm:50051".to_string())
        );
        assert_eq!(meta.salt_hex, Some("0xsalt".to_string()));
    }

    #[test]
    fn to_json_omits_empty_signatures() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        };

        let json = payload.to_json();
        assert!(json.get("signatures").is_none());
    }

    #[test]
    fn to_json_omits_none_metadata() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        };

        let json = payload.to_json();
        assert!(json.get("metadata").is_none());
    }

    #[test]
    fn metadata_serialization_omits_empty_fields() {
        let meta = ProposalMetadataPayload {
            new_threshold: Some(2),
            ..Default::default()
        };

        let json = serde_json::to_value(&meta).unwrap();
        assert!(json.get("new_threshold").is_some());
        assert!(json.get("salt_hex").is_none());
        assert!(json.get("recipient_hex").is_none());
        assert!(json.get("signer_commitments_hex").is_none());
    }

    #[test]
    fn metadata_deserialization_handles_missing_fields() {
        let json = r#"{"new_threshold": 2}"#;
        let meta: ProposalMetadataPayload = serde_json::from_str(json).unwrap();

        assert_eq!(meta.new_threshold, Some(2));
        assert!(meta.signer_commitments_hex.is_empty());
        assert!(meta.salt_hex.is_none());
    }
}
