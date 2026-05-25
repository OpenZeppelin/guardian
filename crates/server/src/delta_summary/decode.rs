//! Decoders for the inputs that the push-time metadata pipeline
//! consumes: the persisted `TransactionSummary` blob and the proposal
//! metadata block that lives in `delta_proposals.delta_payload`.
//!
//! These are pure decoding helpers — they do not classify, project, or
//! orchestrate. See `build.rs` for the orchestrator.

use guardian_shared::FromJson;
use miden_protocol::transaction::TransactionSummary;
use serde_json::Value;

use super::ProposalMetadata;

/// Decode a [`TransactionSummary`] from a persisted `delta_payload`
/// value.
///
/// Handles both on-disk shapes per `research.md` Decision 10:
///
///   - **Wrapper** (multisig pre-execute): `{ tx_summary: {data: base64}, metadata: {..}, signatures?: [..] }`.
///     The function unwraps to `tx_summary` and decodes.
///   - **Raw** (post-execute, single-key, EVM bridge): `{ data: base64 }`.
///     Decoded directly.
///
/// Returns `Err` (a short stable token string) for anything that is not
/// a recognized shape or fails base64 / binary deserialization. Callers
/// at the push-time write path can treat that as "no metadata derivable"
/// and persist `metadata = NULL`.
pub fn decode_transaction_summary(payload: &Value) -> Result<TransactionSummary, &'static str> {
    let candidate = payload.get("tx_summary").unwrap_or(payload);
    TransactionSummary::from_json(candidate).map_err(classify_decode_error)
}

/// Extract the [`ProposalMetadata`] block from a proposal's persisted
/// `delta_payload` value. Returns `None` when no metadata block is
/// present (single-key push deltas) or when the block is malformed.
///
/// Input is the proposal's `delta_payload` (the wrapper shape produced
/// by `normalize_payload` in `crates/server/src/services/mod.rs`), so
/// metadata lives at `delta_payload.metadata`.
pub fn decode_proposal_metadata(proposal_payload: &Value) -> Option<ProposalMetadata> {
    let metadata_value = proposal_payload.get("metadata")?;
    if metadata_value.is_null() {
        return None;
    }
    // The wire shape is field-for-field compatible with our typed
    // struct (snake_case), so serde does the work. A malformed sub-field
    // just makes the whole block None — listing endpoints fall back to
    // derived-only metadata in that case.
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
