//! Payload types for multisig transaction proposals.

use miden_objects::transaction::TransactionSummary;
use private_state_manager_shared::{DeltaSignature, ProposalSignature, ToJson};
use serde::{Deserialize, Serialize};

use crate::keystore::KeyManager;

/// Metadata for multisig transaction proposals.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ProposalMetadataPayload {
    pub proposal_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_threshold: Option<u64>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_commitments: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub salt: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub faucet_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub note_ids: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_psm_pubkey: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_psm_endpoint: Option<String>,
}

/// Complete payload for a multisig transaction proposal.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProposalPayload {
    pub tx_summary: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signatures: Vec<DeltaSignature>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ProposalMetadataPayload>,
}

impl ProposalPayload {
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

    /// Sets the metadata for adding a signer.
    pub fn with_add_signer_metadata(
        mut self,
        new_threshold: u64,
        signer_commitments: Vec<String>,
        salt: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            proposal_type: "add_signer".to_string(),
            target_threshold: Some(new_threshold),
            signer_commitments,
            salt: Some(salt),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for removing a signer.
    pub fn with_remove_signer_metadata(
        mut self,
        new_threshold: u64,
        signer_commitments: Vec<String>,
        salt: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            proposal_type: "remove_signer".to_string(),
            target_threshold: Some(new_threshold),
            signer_commitments,
            salt: Some(salt),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for changing threshold.
    pub fn with_threshold_metadata(
        mut self,
        new_threshold: u64,
        signer_commitments: Vec<String>,
        salt: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            proposal_type: "change_threshold".to_string(),
            target_threshold: Some(new_threshold),
            signer_commitments,
            salt: Some(salt),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for P2ID payment transfers.
    pub fn with_payment_metadata(
        mut self,
        recipient_id: String,
        faucet_id: String,
        amount: u64,
        salt: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            proposal_type: "p2id".to_string(),
            recipient_id: Some(recipient_id),
            faucet_id: Some(faucet_id),
            amount: Some(amount.to_string()),
            salt: Some(salt),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for note consumption transactions.
    pub fn with_note_consumption_metadata(mut self, note_ids: &[String], salt: String) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            proposal_type: "consume_notes".to_string(),
            note_ids: note_ids.to_vec(),
            salt: Some(salt),
            ..Default::default()
        });
        self
    }

    /// Sets the metadata for PSM update transactions.
    pub fn with_psm_update_metadata(
        mut self,
        new_psm_pubkey: String,
        new_psm_endpoint: String,
        salt: String,
    ) -> Self {
        self.metadata = Some(ProposalMetadataPayload {
            proposal_type: "switch_psm".to_string(),
            new_psm_pubkey: Some(new_psm_pubkey),
            new_psm_endpoint: Some(new_psm_endpoint),
            salt: Some(salt),
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
                proposal_type: "add_signer".to_string(),
                target_threshold: Some(2),
                signer_commitments: vec!["0xabc".to_string(), "0xdef".to_string()],
                salt: Some("0x456".to_string()),
                ..Default::default()
            }),
        };

        let json = payload.to_json();

        assert!(json.get("tx_summary").is_some());
        assert!(json.get("signatures").is_some());
        assert!(json.get("metadata").is_some());

        let metadata = json.get("metadata").unwrap();
        assert_eq!(metadata.get("target_threshold").unwrap().as_u64(), Some(2));
        assert_eq!(
            metadata.get("proposal_type").unwrap().as_str(),
            Some("add_signer")
        );
    }

    #[test]
    fn with_add_signer_metadata_sets_fields() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        }
        .with_add_signer_metadata(
            3,
            vec!["0xabc".to_string(), "0xdef".to_string()],
            "0xsalt".to_string(),
        );

        let meta = payload.metadata.unwrap();
        assert_eq!(meta.proposal_type, "add_signer");
        assert_eq!(meta.target_threshold, Some(3));
        assert_eq!(meta.signer_commitments.len(), 2);
        assert_eq!(meta.salt, Some("0xsalt".to_string()));
    }

    #[test]
    fn with_remove_signer_metadata_sets_fields() {
        let payload = ProposalPayload {
            tx_summary: serde_json::json!({}),
            signatures: vec![],
            metadata: None,
        }
        .with_remove_signer_metadata(2, vec!["0xabc".to_string()], "0xsalt".to_string());

        let meta = payload.metadata.unwrap();
        assert_eq!(meta.proposal_type, "remove_signer");
        assert_eq!(meta.target_threshold, Some(2));
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
        assert_eq!(meta.proposal_type, "p2id");
        assert_eq!(meta.recipient_id, Some("0xrecipient".to_string()));
        assert_eq!(meta.faucet_id, Some("0xfaucet".to_string()));
        assert_eq!(meta.amount, Some("1000".to_string()));
        assert_eq!(meta.salt, Some("0xsalt".to_string()));
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
        assert_eq!(meta.proposal_type, "consume_notes");
        assert_eq!(meta.note_ids.len(), 2);
        assert_eq!(meta.note_ids[0], "0xnote1");
        assert_eq!(meta.salt, Some("0xsalt".to_string()));
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
        assert_eq!(meta.proposal_type, "switch_psm");
        assert_eq!(meta.new_psm_pubkey, Some("0xpubkey".to_string()));
        assert_eq!(
            meta.new_psm_endpoint,
            Some("http://new-psm:50051".to_string())
        );
        assert_eq!(meta.salt, Some("0xsalt".to_string()));
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
            proposal_type: "add_signer".to_string(),
            target_threshold: Some(2),
            ..Default::default()
        };

        let json = serde_json::to_value(&meta).unwrap();
        assert!(json.get("target_threshold").is_some());
        assert!(json.get("salt").is_none());
        assert!(json.get("recipient_id").is_none());
    }

    #[test]
    fn metadata_deserialization_handles_missing_fields() {
        let json = r#"{"proposal_type": "add_signer", "target_threshold": 2}"#;
        let meta: ProposalMetadataPayload = serde_json::from_str(json).unwrap();

        assert_eq!(meta.proposal_type, "add_signer");
        assert_eq!(meta.target_threshold, Some(2));
        assert!(meta.signer_commitments.is_empty());
        assert!(meta.salt.is_none());
    }
}
