use serde_json::Value;
use std::sync::Arc;

use guardian_shared::SignatureScheme;

use crate::delta_object::DeltaObject;
use crate::delta_summary::ModuleApproval;
use crate::error::Result;
use crate::metadata::AccountMetadata;
use crate::modules::HookName;
use crate::modules::request::{self, StateRef, TransactionInput};
use crate::network::AppliedState;
use crate::state::AppState;

/// A delta verified against the account's current state and acknowledged by Guardian, not yet
/// committed anywhere.
pub(crate) struct AcknowledgedDelta {
    pub delta: DeltaObject,
    pub applied: AppliedState,
    pub matched_proposal: bool,
}

/// The cosigner a Guardian acknowledgment is evaluated for, the one calling `push_delta` or the
/// one whose request started a Guardian execution, and the account it acts on.
pub(crate) struct Submitter<'a> {
    pub commitment: &'a str,
    /// Whether the request was signed by a delegated session key rather than the wallet.
    pub session: bool,
    pub account: &'a AccountMetadata,
}

/// Verifies `delta` against the state it builds on, runs the policy modules, derives its
/// metadata, and signs Guardian's acknowledgment. The base is the canonical state for a Guardian
/// execution and the queue tail for a pushed delta. Persists nothing and sets no
/// pending-candidate flag: the public push path commits the result itself, and a Guardian
/// execution admits it only at its boundary commit.
pub(crate) async fn acknowledge_delta(
    state: &AppState,
    scheme: &SignatureScheme,
    base: StateRef<'_>,
    delta: &DeltaObject,
    submitter: Submitter<'_>,
) -> Result<AcknowledgedDelta> {
    let applied = apply_to_base(state, base.commitment, base.json, delta).await?;
    let proposal_match =
        lookup_matching_proposal(state, &delta.account_id, delta.nonce, &delta.delta_payload).await;
    acknowledge_applied(
        state,
        scheme,
        base,
        delta,
        applied,
        submitter,
        &proposal_match,
    )
    .await
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

/// Runs the `before_transaction_approved` policy modules on a delta already applied by
/// [`apply_to_base`], derives its metadata, lifting operator intent from the matching proposal
/// when one matched, and signs Guardian's acknowledgment of it. Every Guardian ack is produced
/// here, so no caller acknowledges a delta the modules have not approved.
pub(crate) async fn acknowledge_applied(
    state: &AppState,
    scheme: &SignatureScheme,
    base: StateRef<'_>,
    delta: &DeltaObject,
    applied: AppliedState,
    submitter: Submitter<'_>,
    proposal_match: &ProposalMatch,
) -> Result<AcknowledgedDelta> {
    let approval = approve(state, base, delta, &applied, submitter, proposal_match).await?;
    let matching_proposal = proposal_match.payload();
    let mut derived_metadata =
        crate::delta_summary::build_metadata(&delta.delta_payload, matching_proposal);
    match (derived_metadata.as_mut(), approval) {
        (Some(metadata), approval) => metadata.modules = approval,
        (None, Some(approval)) => tracing::warn!(
            account_id = %delta.account_id,
            catalog_hash = %approval.catalog_hash,
            params_version = approval.params_version,
            "Delta has no metadata to record its module approval in"
        ),
        (None, None) => {}
    }

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

/// The `before_transaction_approved` decision for `delta`, or `None` when no module applies to
/// the account. A proposal lookup that failed is a module failure, never an absent proposer.
async fn approve(
    state: &AppState,
    base: StateRef<'_>,
    delta: &DeltaObject,
    applied: &AppliedState,
    submitter: Submitter<'_>,
    proposal_match: &ProposalMatch,
) -> Result<Option<ModuleApproval>> {
    let Some(modules) = state.modules.as_deref() else {
        return Ok(None);
    };
    let Some(plan) = modules
        .plan(HookName::BeforeTransactionApproved, &delta.account_id)
        .await?
    else {
        return Ok(None);
    };
    let proposal = match proposal_match {
        ProposalMatch::Found(proposal) => Ok(request::proposal_context(proposal)),
        ProposalMatch::Missing => Ok(None),
        ProposalMatch::Unavailable(error) => Err(format!("proposal lookup failed: {error}")),
    };
    let proposer = proposal
        .as_ref()
        .ok()
        .and_then(|context| context.as_ref())
        .map(|context| context.proposer.clone())
        .unwrap_or_default();
    let signed_at = state.clock.now();
    let views = plan.views();
    plan.run(|| {
        request::transaction_request(
            state.network_client.as_ref(),
            &views,
            TransactionInput {
                account_id: &delta.account_id,
                submitter: submitter.commitment,
                submitter_session: submitter.session,
                auth: &submitter.account.auth,
                network_config: &submitter.account.network_config,
                before: base,
                after: StateRef {
                    commitment: &applied.commitment,
                    json: &applied.state_json,
                },
                tx_summary: &delta.delta_payload,
                proposal: proposal?,
                accounting_time: signed_at,
            },
        )
    })
    .await
    .map(|evaluation| {
        Some(ModuleApproval {
            catalog_hash: evaluation.catalog_hash,
            params_version: evaluation.params_version,
            failed_open: evaluation.failed_open,
            signed_at: signed_at.to_rfc3339(),
            submitter: submitter.commitment.to_string(),
            proposer,
        })
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
