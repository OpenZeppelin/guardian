use serde_json::Value;
use std::sync::Arc;

use guardian_shared::SignatureScheme;

use crate::delta_object::DeltaObject;
use crate::delta_summary::ModuleApproval;
use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::modules::HookName;
use crate::modules::pipeline::Plan;
use crate::modules::request::{self, StateRef, TransactionInput};
use crate::network::{AppliedState, ModuleInputs};
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
    ensure_labelled_with_applied_nonce(delta, &applied.state)?;
    let proposal_match =
        lookup_matching_proposal(state, &delta.account_id, delta.nonce, &delta.delta_payload).await;
    acknowledge_applied(state, scheme, delta, applied, submitter, &proposal_match).await
}

/// A delta applied to the state it builds on, with the `before_transaction_approved` plan for
/// the account and, when there is one, the inputs its modules receive, built from the same
/// decode. Only [`ApprovalPlan::applied`] builds it, so no caller can reach the ack with a delta
/// whose modules were left out.
pub(crate) struct Applied<'a> {
    pub state: AppliedState,
    modules: Option<(Plan<'a>, ModuleInputs)>,
}

/// The `before_transaction_approved` plan for an account, read before its delta is applied so
/// the reconstruction task that applies it knows whether to build the module inputs from the
/// same decode.
pub(crate) struct ApprovalPlan<'a>(Option<Plan<'a>>);

impl<'a> ApprovalPlan<'a> {
    /// The plan for `account_id`. When a module applies, the chain fee is loaded first, so the
    /// fee note is told apart inside the reconstruction task without a network call.
    pub(crate) async fn for_account(state: &'a AppState, account_id: &str) -> Result<Self> {
        let plan = match state.modules.as_deref() {
            Some(modules) => {
                modules
                    .plan(HookName::BeforeTransactionApproved, account_id)
                    .await?
            }
            None => None,
        };
        if plan.is_some()
            && let Err(error) = state.network_client.load_fee_faucet().await
        {
            tracing::warn!(%error, "Fee faucet unknown; no output note is treated as the fee note");
        }
        Ok(Self(plan))
    }

    /// Whether the apply must also build the module inputs.
    pub(crate) fn wants_inputs(&self) -> bool {
        self.0.is_some()
    }

    /// The applied delta, with the inputs the plan's modules receive. A plan without inputs is
    /// refused as unavailable rather than acknowledged unevaluated.
    pub(crate) fn applied(
        self,
        state: AppliedState,
        inputs: Option<ModuleInputs>,
    ) -> Result<Applied<'a>> {
        match (self.0, inputs) {
            (Some(plan), Some(inputs)) => Ok(Applied {
                state,
                modules: Some((plan, inputs)),
            }),
            (Some(_), None) => {
                tracing::error!("Delta applied without the inputs its modules need; refusing it");
                Err(GuardianError::ModuleUnavailable { module: None })
            }
            (None, _) => Ok(Applied {
                state,
                modules: None,
            }),
        }
    }
}

/// Refuses a delta whose `nonce` is not the account nonce it leaves the account at (issue
/// #518). The nonce is the delta's storage key (`UNIQUE(account_id, nonce)`), the order of the
/// history, `/delta/since` and the candidate queue, and the lookup key of `getDelta` and
/// `abandonCandidate`, so a label the client chose freely (a timestamp, a gap, a value past
/// `i64::MAX`, which Postgres stores as negative) puts the delta out of chain order in every
/// one of them. The rule holds at every queue depth and for every writer of a delta row, so the
/// rows are in chain order by construction. Rows written before the rule keep their labels. A
/// network without account nonces has nothing to compare.
pub(crate) fn ensure_labelled_with_applied_nonce(
    delta: &DeltaObject,
    applied: &AppliedState,
) -> Result<()> {
    match applied.nonce {
        Some(applied_nonce) if applied_nonce != delta.nonce => {
            tracing::info!(
                account_id = %delta.account_id,
                nonce = delta.nonce,
                applied_nonce,
                "Delta is not labelled with the nonce it leaves the account at; refusing it"
            );
            Err(GuardianError::InvalidDelta(format!(
                "delta nonce {} is not the account nonce {applied_nonce} the delta leaves the \
                 account at",
                delta.nonce
            )))
        }
        _ => Ok(()),
    }
}

/// Verifies `delta` against the state it builds on and applies it, without signing anything.
/// When a module applies to the account, the same decode builds what it receives, after the
/// chain fee is loaded, so request assembly decodes nothing.
pub(crate) async fn apply_to_base<'a>(
    state: &'a AppState,
    base_commitment: &str,
    base_state_json: &serde_json::Value,
    delta: &DeltaObject,
) -> Result<Applied<'a>> {
    let plan = ApprovalPlan::for_account(state, &delta.account_id).await?;
    let client = state.network_client.clone();
    let prev_commitment = base_commitment.to_string();
    let prev_state_json = base_state_json.clone();
    let delta_payload = Arc::new(delta.delta_payload.clone());
    let with_inputs = plan.wants_inputs();
    let (applied, inputs) = crate::network::reconstructor()
        .run(move || {
            if with_inputs {
                client
                    .apply_delta_with_module_inputs(
                        Some(&prev_commitment),
                        &prev_state_json,
                        &delta_payload,
                    )
                    .map(|(state, inputs)| (state, Some(inputs)))
            } else {
                client
                    .verify_and_apply_delta(&prev_commitment, &prev_state_json, &delta_payload)
                    .map(|state| (state, None))
            }
        })
        .await?;
    plan.applied(applied, inputs)
}

/// Runs the `before_transaction_approved` policy modules on a delta already applied by
/// [`apply_to_base`], derives its metadata, lifting operator intent from the matching proposal
/// when one matched, and signs Guardian's acknowledgment of it. Every Guardian ack is produced
/// here, so no caller acknowledges a delta the modules have not approved.
pub(crate) async fn acknowledge_applied(
    state: &AppState,
    scheme: &SignatureScheme,
    delta: &DeltaObject,
    applied: Applied<'_>,
    submitter: Submitter<'_>,
    proposal_match: &ProposalMatch,
) -> Result<AcknowledgedDelta> {
    let Applied {
        state: applied,
        modules,
    } = applied;
    let approval = approve(state, delta, modules, submitter, proposal_match).await?;
    let audited = approval.clone();
    let matching_proposal = proposal_match.payload();
    let mut derived_metadata =
        crate::delta_summary::build_metadata(&delta.delta_payload, matching_proposal);
    match (derived_metadata.as_mut(), approval) {
        (Some(metadata), approval) => metadata.modules = approval,
        (None, Some(approval)) => tracing::warn!(
            account_id = %delta.account_id,
            catalog_hash = %approval.catalog_hash,
            params_version = approval.params_version,
            account_params_version = approval.account_params_version,
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
    if let Some(approval) = audited {
        audit_failed_open(state, delta, approval);
    }

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
    delta: &DeltaObject,
    modules: Option<(Plan<'_>, ModuleInputs)>,
    submitter: Submitter<'_>,
    proposal_match: &ProposalMatch,
) -> Result<Option<ModuleApproval>> {
    let Some((plan, inputs)) = modules else {
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
    let network_id = plan.network_id();
    plan.run(|| {
        request::transaction_request(TransactionInput {
            network_id,
            account_id: &delta.account_id,
            submitter: submitter.commitment,
            submitter_session: submitter.session,
            auth: &submitter.account.auth,
            network_config: &submitter.account.network_config,
            inputs,
            proposal: proposal?,
            accounting_time: signed_at,
        })
    })
    .await
    .map(|evaluation| {
        Some(ModuleApproval {
            catalog_hash: evaluation.catalog_hash,
            params_version: evaluation.params_version,
            account_params_version: evaluation.account_params_version,
            failed_open: evaluation.failed_open,
            signed_at: signed_at.to_rfc3339(),
            submitter: submitter.commitment.to_string(),
            proposer,
        })
    })
}

/// Records the failures `on_error = allow` let through for an acknowledgement the Guardian
/// signed, as at proposal creation and registration, so they are on record even when the
/// acknowledged delta is never stored.
fn audit_failed_open(state: &AppState, delta: &DeltaObject, approval: ModuleApproval) {
    let submitter = approval.submitter.clone();
    crate::modules::pipeline::Evaluation {
        catalog_hash: approval.catalog_hash,
        params_version: approval.params_version,
        account_params_version: approval.account_params_version,
        failed_open: approval.failed_open,
    }
    .audit_failed_open(
        state.auditor.as_ref(),
        HookName::BeforeTransactionApproved,
        &delta.account_id,
        &submitter,
        [
            ("nonce".to_string(), delta.nonce.into()),
            (
                "prev_commitment".to_string(),
                delta.prev_commitment.clone().into(),
            ),
        ]
        .into_iter()
        .collect(),
    );
}

/// The `delta_proposals` row matching a delta, as far as storage could tell.
#[derive(Clone)]
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
                    "delta_proposals lookup errored; a multisig push is refused, a delta a \
                     policy module evaluates is refused as unavailable, and a single-key push or \
                     a Guardian execution with no module to run continues without proposal \
                     metadata"
                );
                ProposalMatch::Unavailable(err)
            }
        }
    }
}
