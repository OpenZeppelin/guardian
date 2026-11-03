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
    let applied = apply_to_base(state, base_commitment, base_state_json, delta).await?;
    let proposal_match =
        lookup_matching_proposal(state, &delta.account_id, delta.nonce, &delta.delta_payload).await;
    acknowledge_applied(state, scheme, delta, applied, proposal_match.payload()).await
}

/// Verifies `delta` against the state it builds on and applies it, without signing anything.
pub(crate) async fn apply_to_base(
    state: &AppState,
    base_commitment: &str,
    base_state_json: &serde_json::Value,
    delta: &DeltaObject,
) -> Result<AppliedState> {
    let client = state.network_client.clone();
    let prev_commitment = base_commitment.to_string();
    let prev_state_json = base_state_json.clone();
    let delta_payload = Arc::new(delta.delta_payload.clone());
    crate::network::reconstructor()
        .run(move || {
            client.verify_and_apply_delta(&prev_commitment, &prev_state_json, &delta_payload)
        })
        .await
        .map_err(Into::into)
}

/// Derives the metadata of a delta already applied by [`apply_to_base`], lifting operator
/// intent from `matching_proposal` when one matched, and signs Guardian's acknowledgment of it.
pub(crate) async fn acknowledge_applied(
    state: &AppState,
    scheme: &SignatureScheme,
    delta: &DeltaObject,
    applied: AppliedState,
    matching_proposal: Option<&Value>,
) -> Result<AcknowledgedDelta> {
    let derived_metadata =
        crate::delta_summary::build_metadata(&delta.delta_payload, matching_proposal);

    let mut acknowledged = delta.clone();
    acknowledged.new_commitment = Some(applied.commitment.clone());
    acknowledged.metadata = derived_metadata;
    acknowledged = state.ack.ack_delta(acknowledged, scheme).await?;
    acknowledged.ack_pubkey = state.ack.pubkey(scheme);
    acknowledged.ack_scheme = scheme.as_str().to_string();

    Ok(AcknowledgedDelta {
        delta: acknowledged,
        applied,
        matched_proposal: matching_proposal.is_some(),
    })
}

/// The `delta_proposals` row matching a delta, as far as storage could tell.
pub(crate) enum ProposalMatch {
    Found(Box<DeltaObject>),
    Missing,
    Unavailable(String),
}

impl ProposalMatch {
    /// The matching proposal's payload, which `build_metadata` lifts operator intent from.
    /// A storage fault yields `None`: callers that must not proceed on it refuse first.
    pub(crate) fn payload(&self) -> Option<&Value> {
        match self {
            Self::Found(proposal) => Some(&proposal.delta_payload),
            Self::Missing | Self::Unavailable(_) => None,
        }
    }
}

/// Look up the `delta_proposals` row for a delta. A missing row is [`ProposalMatch::Missing`].
/// A storage fault is [`ProposalMatch::Unavailable`] and is logged at `warn`; the caller
/// decides whether that fault refuses the delta.
pub(crate) async fn lookup_matching_proposal(
    state: &AppState,
    account_id: &str,
    nonce: u64,
    delta_payload: &Value,
) -> ProposalMatch {
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
                return ProposalMatch::Missing;
            }
        }
    };
    match state
        .storage
        .pull_delta_proposal(account_id, &proposal_id)
        .await
    {
        Ok(proposal) => ProposalMatch::Found(Box::new(proposal)),
        Err(err) => {
            if crate::storage::is_storage_not_found(&err) {
                tracing::debug!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    "no matching delta_proposal row (single-key push or unrelated payload)"
                );
                ProposalMatch::Missing
            } else {
                tracing::warn!(
                    account_id = %account_id,
                    nonce,
                    proposal_id = %proposal_id,
                    error = %err,
                    "delta_proposals lookup errored; a multisig push is refused, and a \
                     single-key push or a Guardian execution continues without proposal metadata"
                );
                ProposalMatch::Unavailable(err)
            }
        }
    }
}
