use miden_objects::transaction::TransactionSummary;
use private_state_manager_shared::FromJson;
use private_state_manager_shared::hex::IntoHex;

use crate::DeltaObject;
use crate::error::ClientError;

/// Trait for extracting a TransactionSummary from delta-related types.
pub trait TryIntoTxSummary {
    fn try_into_tx_summary(&self) -> Result<TransactionSummary, ClientError>;
}

impl TryIntoTxSummary for DeltaObject {
    fn try_into_tx_summary(&self) -> Result<TransactionSummary, ClientError> {
        // Parse the delta_payload string as JSON
        let payload_json: serde_json::Value =
            serde_json::from_str(&self.delta_payload).map_err(|e| {
                ClientError::InvalidResponse(format!("Invalid delta_payload JSON: {e}"))
            })?;

        // Try proposal format first (has tx_summary field)
        if let Some(tx_summary_json) = payload_json.get("tx_summary") {
            return TransactionSummary::from_json(tx_summary_json).map_err(|e| {
                ClientError::InvalidResponse(format!("Failed to deserialize tx_summary: {e}"))
            });
        }

        // Fall back to direct format
        TransactionSummary::from_json(&payload_json).map_err(|e| {
            ClientError::InvalidResponse(format!("Failed to deserialize delta_payload: {e}"))
        })
    }
}

/// Returns the commitment of a TransactionSummary as a hex string with 0x prefix.
pub fn tx_summary_commitment_hex(tx_summary: &TransactionSummary) -> String {
    tx_summary.to_commitment().into_hex()
}

#[cfg(test)]
mod tests {
    use super::*;
    use miden_objects::account::delta::{AccountStorageDelta, AccountVaultDelta};
    use miden_objects::account::{AccountDelta, AccountId};
    use miden_objects::transaction::{InputNotes, OutputNotes};
    use miden_objects::{Felt, FieldElement, Word, ZERO};
    use private_state_manager_shared::ToJson;

    fn create_test_tx_summary() -> TransactionSummary {
        // Use a valid hex account ID from the test fixtures
        let account_id =
            AccountId::from_hex("0x8a65fc5a39e4cd106d648e3eb4ab5f").expect("valid account id");

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
            Word::from([ZERO; 4]),
        )
    }

    #[test]
    fn test_extract_tx_summary_direct_format() {
        let tx_summary = create_test_tx_summary();
        let tx_json = tx_summary.to_json();

        let delta = DeltaObject {
            account_id: "0x123".to_string(),
            nonce: 1,
            prev_commitment: "0x000".to_string(),
            delta_payload: serde_json::to_string(&tx_json).unwrap(),
            new_commitment: "0x111".to_string(),
            ack_sig: String::new(),
            candidate_at: String::new(),
            canonical_at: None,
            discarded_at: None,
            status: None,
            ack_pubkey: None,
            ack_scheme: None,
        };

        let extracted = delta.try_into_tx_summary().expect("should extract");
        assert_eq!(tx_summary.to_commitment(), extracted.to_commitment());
    }

    #[test]
    fn test_extract_tx_summary_proposal_format() {
        let tx_summary = create_test_tx_summary();
        let tx_json = tx_summary.to_json();

        let payload = serde_json::json!({
            "tx_summary": tx_json,
            "signatures": []
        });

        let delta = DeltaObject {
            account_id: "0x123".to_string(),
            nonce: 1,
            prev_commitment: "0x000".to_string(),
            delta_payload: serde_json::to_string(&payload).unwrap(),
            new_commitment: "0x111".to_string(),
            ack_sig: String::new(),
            candidate_at: String::new(),
            canonical_at: None,
            discarded_at: None,
            status: None,
            ack_pubkey: None,
            ack_scheme: None,
        };

        let extracted = delta.try_into_tx_summary().expect("should extract");
        assert_eq!(tx_summary.to_commitment(), extracted.to_commitment());
    }

    #[test]
    fn test_tx_summary_commitment_hex() {
        let tx_summary = create_test_tx_summary();
        let hex = tx_summary_commitment_hex(&tx_summary);

        assert!(hex.starts_with("0x"));
        assert_eq!(hex.len(), 2 + 64); // 0x + 32 bytes as hex
    }
}
