use serde_json::Value;
use std::sync::Arc;

use guardian_shared::SignatureScheme;

use crate::delta_object::DeltaObject;
use crate::error::Result;
use crate::network::AppliedState;
use crate::state::AppState;

/// A delta verified against the account's current state and acknowledged by Guardian, not yet
/// committed anywhere.
pub(crate) struct AcknowledgedDelta {
    pub delta: DeltaObject,
    pub applied: AppliedState,
    pub matched_proposal: bool,
}

/// Verifies `delta` against the state it builds on, derives its metadata, and signs Guardian's
/// acknowledgment. The base is the canonical state for a Guardian execution and the queue tail
/// for a pushed delta. Persists nothing and sets no pending-candidate flag: the public push path
/// commits the result itself, and a Guardian execution admits it only at its boundary commit.
pub(crate) async fn acknowledge_delta(
    state: &AppState,
    scheme: &SignatureScheme,
    base_commitment: &str,
    base_state_json: &serde_json::Value,
    delta: &DeltaObject,
) -> Result<AcknowledgedDelta> {
    let applied = {
        let client = state.network_client.clone();
        let prev_commitment = base_commitment.to_string();
        let prev_state_json = base_state_json.clone();
        let delta_payload = Arc::new(delta.delta_payload.clone());
        crate::network::reconstructor()
            .run(move || {
                client.verify_delta(&prev_commitment, &prev_state_json, &delta_payload)?;
                client.apply_delta(&prev_state_json, &delta_payload)
            })
            .await?
    };

    // Unconditional lookup: for multisig pushes this lifts the matching proposal's metadata
    // so `build_metadata` can preserve operator intent. For single-key pushes the lookup
    // misses and returns `None`; the cost is one extra storage read per push.
    let matching_proposal_payload = lookup_matching_proposal_payload(
        state,
        &delta.account_id,
        delta.nonce,
        &delta.delta_payload,
    )
    .await;

    let derived_metadata = crate::delta_summary::build_metadata(
        &delta.delta_payload,
        matching_proposal_payload.as_ref(),
    );

    let mut acknowledged = delta.clone();
    acknowledged.new_commitment = Some(applied.commitment.clone());
    acknowledged.metadata = derived_metadata;
    acknowledged = state.ack.ack_delta(acknowledged, scheme).await?;
    acknowledged.ack_pubkey = state.ack.pubkey(scheme);
    acknowledged.ack_scheme = scheme.as_str().to_string();

    Ok(AcknowledgedDelta {
        delta: acknowledged,
        applied,
        matched_proposal: matching_proposal_payload.is_some(),
    })
}

/// Look up the matching `delta_proposals` row's `delta_payload` for
/// the delta being pushed. Returns `None` when no proposal matches.
/// All failure paths are non-fatal so the push proceeds; the
/// "no match" cases log at `debug`, real storage errors log at `warn`
/// so silent metadata loss stays detectable in production.
async fn lookup_matching_proposal_payload(
    state: &AppState,
    account_id: &str,
    nonce: u64,
    delta_payload: &Value,
) -> Option<Value> {
    let proposal_id = {
        let client = &state.network_client;
        match client.delta_proposal_id(account_id, nonce, delta_payload) {
            Ok(id) => id,
            Err(err) => {
                tracing::debug!(
                    account_id = %account_id,
                    nonce,
                    error = %err,
                    "delta_proposal_id could not compute an id for this payload; \
                     persisting metadata without proposal block (EVM / malformed payload)"
                );
                return None;
            }
        }
    };
    match state
        .storage
        .pull_delta_proposal(account_id, &proposal_id)
        .await
    {
        Ok(proposal) => Some(proposal.delta_payload),
        Err(err) => {
            if crate::storage::is_storage_not_found(&err) {
                tracing::debug!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    "no matching delta_proposal row (single-key push or unrelated payload)"
                );
            } else {
                tracing::warn!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    error = %err,
                    "delta_proposals lookup errored during push_delta metadata derivation; \
                     persisting metadata without proposal block (operator-stated intent lost \
                     until storage recovers — investigate storage backend)"
                );
            }
            None
        }
    }
}
