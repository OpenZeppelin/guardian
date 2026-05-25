//! Pure decoders for the persisted `TransactionSummary` blob and the
//! proposal metadata block. See `build.rs` for the orchestrator.

use guardian_shared::FromJson;
use miden_protocol::transaction::TransactionSummary;
use serde_json::Value;

use super::ProposalMetadata;

/// Decode a [`TransactionSummary`] from a persisted `delta_payload`.
///
/// Handles both shapes: the multisig wrapper `{ tx_summary: {data},
/// metadata, signatures? }` and the raw `{ data: base64 }`. Returns a
/// short stable token string on failure.
pub fn decode_transaction_summary(payload: &Value) -> Result<TransactionSummary, &'static str> {
    let candidate = payload.get("tx_summary").unwrap_or(payload);
    TransactionSummary::from_json(candidate).map_err(classify_decode_error)
}

/// Extract the [`ProposalMetadata`] block from a proposal's persisted
/// `delta_payload`. Returns `None` when no metadata block is present
/// or when the block is malformed.
pub fn decode_proposal_metadata(proposal_payload: &Value) -> Option<ProposalMetadata> {
    let metadata_value = proposal_payload.get("metadata")?;
    if metadata_value.is_null() {
        return None;
    }
    serde_json::from_value::<ProposalMetadata>(metadata_value.clone()).ok()
}

fn classify_decode_error(err: String) -> &'static str {
    if err.contains("Base64") {
        "malformed_base64"
    } else if err.contains("Missing or invalid 'data' field") {
        "missing_data_field"
    } else {
        "malformed_tx_summary"
    }
}
