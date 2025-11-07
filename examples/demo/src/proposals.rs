use private_state_manager_client::DeltaObject;
use private_state_manager_shared::FromJson;
use miden_objects::transaction::TransactionSummary;
use serde_json::Value;

/// Extract proposal metadata from a delta object
pub fn extract_proposal_metadata(delta: &DeltaObject) -> ProposalMetadata {
    if let Ok(payload_json) = serde_json::from_str::<Value>(&delta.delta_payload) {
        if let Some(tx_summary) = payload_json.get("tx_summary") {
            return ProposalMetadata {
                proposal_type: "update_signers".to_string(),
                tx_summary: Some(tx_summary.clone()),
                new_threshold: payload_json
                    .get("new_threshold")
                    .and_then(|v| v.as_u64()),
                signer_commitments_hex: payload_json
                    .get("signer_commitments_hex")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                signers_required_hex: payload_json
                    .get("signers_required_hex")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default(),
                salt_hex: payload_json
                    .get("salt_hex")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            };
        }
    }
    ProposalMetadata::default()
}

#[derive(Debug, Clone, Default)]
pub struct ProposalMetadata {
    pub proposal_type: String,
    pub tx_summary: Option<Value>,
    pub new_threshold: Option<u64>,
    pub signer_commitments_hex: Vec<String>,
    pub signers_required_hex: Vec<String>,
    pub salt_hex: Option<String>,
}

impl ProposalMetadata {
    /// Get the transaction summary commitment from the metadata
    /// This deserializes the TransactionSummary and computes its commitment
    pub fn get_tx_commitment(&self) -> Option<String> {
        self.tx_summary.as_ref().and_then(|tx_json| {
            // Deserialize the TransactionSummary from JSON
            TransactionSummary::from_json(tx_json).ok().map(|tx| {
                let commitment = tx.to_commitment();
                format!("0x{}", hex::encode(commitment.as_bytes()))
            })
        })
    }

    /// Check if all required signatures have been collected
    pub fn is_ready(&self, signature_count: usize) -> bool {
        signature_count >= self.signers_required_hex.len()
    }

    /// Convert hex string to Word
    fn hex_to_word(hex: &str) -> miden_objects::Word {
        let hex = hex.strip_prefix("0x").unwrap_or(hex);
        let bytes = hex::decode(hex).expect("Invalid hex");
        let mut word = [0u64; 4];
        for (i, chunk) in bytes.chunks(8).enumerate() {
            let mut arr = [0u8; 8];
            arr[..chunk.len()].copy_from_slice(chunk);
            word[i] = u64::from_le_bytes(arr);
        }
        miden_objects::Word::from(word.map(miden_objects::Felt::new))
    }

    /// Get salt as Word
    pub fn salt(&self) -> miden_objects::Word {
        self.salt_hex
            .as_ref()
            .map(|s| Self::hex_to_word(s))
            .unwrap_or_else(|| miden_objects::Word::from([miden_objects::Felt::new(0); 4]))
    }

    /// Get signer commitments as Vec<Word>
    pub fn signer_commitments(&self) -> Vec<miden_objects::Word> {
        self.signer_commitments_hex
            .iter()
            .map(|h| Self::hex_to_word(h))
            .collect()
    }
}


/// Count signatures in a pending proposal
pub fn count_signatures(delta: &DeltaObject) -> usize {
    if let Some(ref status) = delta.status {
        if let Some(ref status_oneof) = status.status {
            use private_state_manager_client::delta_status::Status;
            if let Status::Pending(ref pending) = status_oneof {
                return pending.cosigner_sigs.len();
            }
        }
    }
    0
}

/// Check if a specific signer has already signed
pub fn has_signer_signed(delta: &DeltaObject, signer_id: &str) -> bool {
    if let Some(ref status) = delta.status {
        if let Some(ref status_oneof) = status.status {
            use private_state_manager_client::delta_status::Status;
            if let Status::Pending(ref pending) = status_oneof {
                return pending.cosigner_sigs.iter().any(|sig| sig.signer_id == signer_id);
            }
        }
    }
    false
}

/// Get list of signers who have signed
pub fn get_signers(delta: &DeltaObject) -> Vec<String> {
    if let Some(ref status) = delta.status {
        if let Some(ref status_oneof) = status.status {
            use private_state_manager_client::delta_status::Status;
            if let Status::Pending(ref pending) = status_oneof {
                return pending.cosigner_sigs.iter().map(|sig| sig.signer_id.clone()).collect();
            }
        }
    }
    Vec::new()
}