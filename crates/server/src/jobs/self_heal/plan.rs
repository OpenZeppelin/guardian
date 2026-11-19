//! The planner: pure, synchronous and CPU-bound. It reads no storage and
//! makes no RPC call.

use guardian_shared::SignatureScheme;

use crate::metadata::auth::Auth;
use crate::network::{AppliedState, NetworkClient};
use crate::state_object::StateObject;
use crate::storage::AccountHealWrite;

use super::deltas::DeltaJudge;
use super::proposals::ProposalJudge;
use super::replay::{ReplayOutcome, Step, build_base, replay};
use super::rows::{Row, Rows, same_commitment};
use super::types::{AccountInputs, AccountPlan, ChainRead, HealPlan, RecoveryReason};
use super::unrecovered::{Recovery, signers_of};

const SWITCH_GUARDIAN: &str = "switch_guardian";

/// Decide what to do with one account whose stored commitment is not the
/// on-chain commitment.
pub fn plan_account(client: &dyn NetworkClient, inputs: AccountInputs) -> AccountPlan {
    let rows = Rows::new(&inputs);
    match &inputs.chain {
        ChainRead::Mismatch { commitment } => plan_mismatch(client, &inputs, &rows, commitment),
        ChainRead::Absent => plan_absent(client, &inputs, &rows),
    }
}

fn plan_mismatch(
    client: &dyn NetworkClient,
    inputs: &AccountInputs,
    rows: &Rows<'_>,
    target: &str,
) -> AccountPlan {
    match replay(client, &inputs.metadata.account_id, rows, target) {
        ReplayOutcome::Reached(path) => plan_heal(client, inputs, rows, &path),
        ReplayOutcome::NoBase => Recovery {
            reason: RecoveryReason::NoBase,
            detail: Some("no row builds the account from a creation delta".to_string()),
            creation: None,
            trace: &[],
            needed: Some(rows.lowest_nonce_base().unwrap_or(target).to_string()),
        }
        .into_plan(inputs, rows),
        ReplayOutcome::Short {
            deepest,
            canonical_link_failed,
            budget_exceeded,
        } => {
            let reason = short_reason(
                inputs,
                rows,
                &deepest,
                canonical_link_failed,
                budget_exceeded,
            );
            Recovery {
                reason,
                detail: Some(short_detail(reason).to_string()),
                creation: deepest.first(),
                trace: &deepest,
                needed: Some(target.to_string()),
            }
            .into_plan(inputs, rows)
        }
    }
}

fn short_reason(
    inputs: &AccountInputs,
    rows: &Rows<'_>,
    deepest: &[Step<'_>],
    canonical_link_failed: bool,
    budget_exceeded: bool,
) -> RecoveryReason {
    let last = deepest.last().map(|step| step.applied.commitment.as_str());
    if budget_exceeded {
        RecoveryReason::AmbiguousHistory
    } else if canonical_link_failed {
        RecoveryReason::UndecodableRow
    } else if last.is_some_and(|last| switches_guardian_at(inputs, rows, last)) {
        RecoveryReason::LeftThisGuardian
    } else {
        RecoveryReason::ReplayShort
    }
}

fn short_detail(reason: RecoveryReason) -> &'static str {
    match reason {
        RecoveryReason::AmbiguousHistory => {
            "the replay used its whole budget of delta applications"
        }
        RecoveryReason::UndecodableRow => "a canonical row that links the replay did not apply",
        RecoveryReason::LeftThisGuardian => {
            "the replay stops where a switch_guardian row or proposal starts"
        }
        RecoveryReason::ReplayShort
        | RecoveryReason::NoBase
        | RecoveryReason::CreationDeltaPending
        | RecoveryReason::RpcError => "the rows end before the on-chain commitment",
    }
}

/// Whether a stored row or proposal that switches the guardian starts at
/// `commitment`.
fn switches_guardian_at(inputs: &AccountInputs, rows: &Rows<'_>, commitment: &str) -> bool {
    let row_switches = rows
        .based_on(commitment)
        .iter()
        .any(|row| row.delta.proposal_type() == Some(SWITCH_GUARDIAN));
    let proposal_switches = inputs.proposals.iter().any(|record| {
        same_commitment(&record.proposal.prev_commitment, commitment)
            && record.proposal.proposal_type() == Some(SWITCH_GUARDIAN)
    });
    row_switches || proposal_switches
}

fn plan_heal(
    client: &dyn NetworkClient,
    inputs: &AccountInputs,
    rows: &Rows<'_>,
    path: &[Step<'_>],
) -> AccountPlan {
    let Some(true_state) = path.last().map(|step| &step.applied) else {
        return Recovery {
            reason: RecoveryReason::NoBase,
            detail: Some("the replay returned an empty path".to_string()),
            creation: None,
            trace: &[],
            needed: None,
        }
        .into_plan(inputs, rows);
    };
    match true_auth(client, true_state) {
        Ok(auth) => AccountPlan::Heal(Box::new(heal_plan(client, inputs, path, true_state, auth))),
        Err(detail) => Recovery {
            reason: RecoveryReason::UndecodableRow,
            detail: Some(detail),
            creation: path.first(),
            trace: path,
            needed: Some(true_state.commitment.clone()),
        }
        .into_plan(inputs, rows),
    }
}

fn heal_plan(
    client: &dyn NetworkClient,
    inputs: &AccountInputs,
    path: &[Step<'_>],
    true_state: &AppliedState,
    auth: Auth,
) -> HealPlan {
    let deltas = DeltaJudge {
        client,
        inputs,
        path,
    }
    .judge(true_state);
    let proposals = ProposalJudge {
        client,
        frontier: &deltas.frontier,
        true_state,
    }
    .judge(&inputs.proposals);
    let mut actions = deltas.actions;
    actions.extend(proposals.actions);
    HealPlan {
        write: AccountHealWrite {
            account_id: inputs.metadata.account_id.clone(),
            expected_state_commitment: inputs.stored.commitment.clone(),
            state: true_state_object(inputs, true_state),
            auth,
            has_pending_candidate: deltas.has_pending_candidate,
            delta_writes: deltas.writes,
            proposal_writes: proposals.writes,
            now: inputs.now.clone(),
        },
        guardian_commitment: true_state.guardian_commitment.clone(),
        true_signers: signers_of(true_state),
        actions,
    }
}

fn true_state_object(inputs: &AccountInputs, true_state: &AppliedState) -> StateObject {
    StateObject {
        account_id: inputs.metadata.account_id.clone(),
        state_json: true_state.state_json.clone(),
        commitment: true_state.commitment.clone(),
        nonce: true_state.nonce,
        created_at: inputs.stored.created_at.clone(),
        updated_at: inputs.now.clone(),
        auth_scheme: String::new(),
    }
}

/// The auth `/configure` demands for the true state: its cosigners, each
/// under the scheme the state stores for it, and the guardian scheme the
/// state stores. Nothing comes from the stored auth, which is forged.
fn true_auth(client: &dyn NetworkClient, true_state: &AppliedState) -> Result<Auth, String> {
    client
        .validate_multisig_auth(&true_state.state_json)
        .map_err(|error| format!("the true state is not a valid guarded multisig: {error}"))?;
    let guardian_scheme = client
        .account_auth_binding(&true_state.state_json)
        .map_err(|error| format!("the true state's auth binding does not decode: {error}"))?
        .and_then(|binding| binding.guardian_scheme)
        .ok_or_else(|| "the true state stores no supported guardian scheme".to_string())?;
    let auth = true_state
        .updated_auth(&empty_auth(guardian_scheme))
        .ok_or_else(|| "the true state carries no cosigners".to_string())?;
    auth.validate_canonical_signer_set()
        .map_err(|error| format!("the true state's signer set is not usable: {error}"))?;
    Ok(auth)
}

/// A single-scheme auth with no cosigners. `Auth::with_chain_cosigners`
/// on it keeps the single-scheme form while every cosigner uses the
/// guardian scheme, as `/configure` stores such an account.
fn empty_auth(guardian_scheme: SignatureScheme) -> Auth {
    match guardian_scheme {
        SignatureScheme::Falcon => Auth::MidenFalconRpo {
            cosigner_commitments: Vec::new(),
        },
        SignatureScheme::Ecdsa => Auth::MidenEcdsa {
            cosigner_commitments: Vec::new(),
        },
    }
}

fn plan_absent(client: &dyn NetworkClient, inputs: &AccountInputs, rows: &Rows<'_>) -> AccountPlan {
    if rows.all().iter().any(Row::is_canonical) {
        return Recovery {
            reason: RecoveryReason::RpcError,
            detail: Some(
                "the chain reports no state for an account with canonical history".to_string(),
            ),
            creation: None,
            trace: &[],
            needed: None,
        }
        .into_plan(inputs, rows);
    }
    let creations = creation_steps(client, inputs, rows);
    let Some(first) = creations.first() else {
        return AccountPlan::Skip {
            reason: "the account is not deployed and has no creation delta",
        };
    };
    let pending_from_stored = creations
        .iter()
        .any(|step| same_commitment(&step.row.delta.prev_commitment, &inputs.stored.commitment));
    if pending_from_stored {
        return AccountPlan::Skip {
            reason: "the creation delta is not yet on chain and starts from the stored state",
        };
    }
    Recovery {
        reason: RecoveryReason::CreationDeltaPending,
        detail: Some(
            "the creation delta is not yet on chain and does not start from the stored state"
                .to_string(),
        ),
        creation: Some(first),
        trace: &[],
        needed: Some(first.row.delta.prev_commitment.clone()),
    }
    .into_plan(inputs, rows)
}

fn creation_steps<'a>(
    client: &dyn NetworkClient,
    inputs: &AccountInputs,
    rows: &Rows<'a>,
) -> Vec<Step<'a>> {
    rows.all()
        .iter()
        .filter_map(|row| {
            build_base(client, &inputs.metadata.account_id, row)
                .map(|applied| Step { row: *row, applied })
        })
        .collect()
}
