use std::sync::Arc;

use chrono::Utc;

use super::executor::{ExecutionInput, ProposalExecutor};
use super::worker::{ExecutionJob, run_execution};
use crate::config::execution::ExecutionConfig;
use crate::coordination::{ExecutionLeases, InMemoryExecutionLeases, release_quietly};
use crate::delta_object::DeltaStatus;
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::execution_status::ExecutionEnvelope;
use crate::services::resolve_account;
use crate::state::AppState;
use crate::storage::{ExecutionRecord, LeaseFence, NewExecutionReservation, ReservationWrite};

/// Everything the server needs to offer Guardian execution.
#[derive(Clone)]
pub struct ExecutionState {
    pub config: ExecutionConfig,
    pub leases: Arc<dyn ExecutionLeases>,
    pub executor: Option<Arc<dyn ProposalExecutor>>,
    pub replica_id: String,
}

impl Default for ExecutionState {
    fn default() -> Self {
        Self {
            config: ExecutionConfig::default(),
            leases: Arc::new(InMemoryExecutionLeases::new()),
            executor: None,
            replica_id: "single-process".to_string(),
        }
    }
}

impl ExecutionState {
    /// The executor, when this server offers execution at all. The builder installs one only
    /// when a prover is configured, proving is enabled and the build includes it.
    fn available(&self, canonicalization_enabled: bool) -> Result<Arc<dyn ProposalExecutor>> {
        if !canonicalization_enabled {
            return Err(GuardianError::ProvingUnavailable);
        }
        self.executor
            .clone()
            .ok_or(GuardianError::ProvingUnavailable)
    }

    fn new_holder_id(&self) -> String {
        let mut suffix = [0u8; 8];
        rand::RngExt::fill(&mut rand::rng(), &mut suffix);
        format!("{}:{}", self.replica_id, hex::encode(suffix))
    }
}

#[derive(Debug, Clone)]
pub struct RequestExecutionParams {
    pub account_id: String,
    pub proposal_id: String,
    pub credentials: Credentials,
}

/// Requests Guardian execution of a threshold-met proposal. Refusals that can be decided now
/// return synchronously and create nothing; everything else runs in the background under a
/// durable reservation.
#[tracing::instrument(
    level = "info",
    skip(state, params),
    fields(account_id = %params.account_id, proposal_id = %params.proposal_id)
)]
pub async fn request_execution(
    state: &AppState,
    params: RequestExecutionParams,
) -> Result<ExecutionEnvelope> {
    let RequestExecutionParams {
        account_id,
        proposal_id,
        credentials,
    } = params;
    let resolved = resolve_account(state, &account_id, &credentials).await?;
    ensure_account_active_metadata(&resolved.metadata)?;
    let executor = state
        .execution
        .available(state.canonicalization.is_some())?;

    let proposal = state
        .storage
        .pull_delta_proposal(&account_id, &proposal_id)
        .await
        .map_err(|_| GuardianError::ProposalNotFound {
            account_id: account_id.clone(),
            commitment: proposal_id.clone(),
        })?;
    let DeltaStatus::Pending { cosigner_sigs, .. } = &proposal.status else {
        return Err(GuardianError::ProposalNotFound {
            account_id: account_id.clone(),
            commitment: proposal_id.clone(),
        });
    };

    if let Some(active) = state
        .storage
        .load_active_execution(&account_id)
        .await
        .map_err(GuardianError::StorageError)?
    {
        return answer_active(active, &proposal_id);
    }
    if proposal.delta_payload.get("transaction_request").is_none() {
        return Err(GuardianError::ProposalMissingTransactionRequest);
    }
    if state
        .storage
        .has_pending_candidate(&account_id)
        .await
        .map_err(GuardianError::StorageError)?
    {
        return Err(GuardianError::ConflictPendingDelta);
    }

    let current_state = state
        .storage
        .pull_state(&account_id)
        .await
        .map_err(|_| GuardianError::StateNotFound(account_id.clone()))?;
    let input = ExecutionInput {
        account_id: account_id.clone(),
        state_json: current_state.state_json,
        proposal_payload: proposal.delta_payload,
        cosigner_signatures: cosigner_sigs.clone(),
    };
    let selection = executor.select_signatures(&input)?;
    if !selection.is_ready() {
        return Err(GuardianError::ProposalNotReady {
            required: selection.required,
            valid: selection.valid,
        });
    }

    let holder_id = state.execution.new_holder_id();
    let elector = state.execution.leases.elector(&account_id, &holder_id);
    let lease_ttl = state.execution.config.lease;
    let Some(lease) = elector
        .try_acquire(lease_ttl)
        .await
        .map_err(|e| GuardianError::StorageError(e.to_string()))?
    else {
        return already_executing(state, &account_id, proposal_id).await;
    };
    let fence = LeaseFence::from(&lease);
    let now = Utc::now();
    let reservation = NewExecutionReservation {
        account_id: account_id.clone(),
        proposal_id: proposal_id.clone(),
        fence: fence.clone(),
        lease_expires_at: lease.expires_at,
        ignored_signatures: selection.ignored,
        now,
    };
    let attempt = match state
        .storage
        .create_execution_reservation(reservation)
        .await
        .map_err(GuardianError::StorageError)?
    {
        ReservationWrite::Created { attempt } => attempt,
        ReservationWrite::AlreadyReserved {
            proposal_id: blocking,
            ..
        } => {
            release_quietly(elector.as_ref(), lease).await;
            if blocking == proposal_id {
                let record = state
                    .storage
                    .load_active_execution(&account_id)
                    .await
                    .map_err(GuardianError::StorageError)?
                    .ok_or_else(|| {
                        GuardianError::StorageError(
                            "the reservation that refused this request has vanished".to_string(),
                        )
                    })?;
                return Ok(ExecutionEnvelope::from_record(&record, true, false));
            }
            return Err(GuardianError::ExecutionConflict {
                blocking_proposal_id: blocking,
            });
        }
        ReservationWrite::CandidateExists => {
            release_quietly(elector.as_ref(), lease).await;
            return Err(GuardianError::ConflictPendingDelta);
        }
        ReservationWrite::StaleLease => {
            release_quietly(elector.as_ref(), lease).await;
            return already_executing(state, &account_id, proposal_id).await;
        }
    };

    let record = state
        .storage
        .load_active_execution(&account_id)
        .await
        .map_err(GuardianError::StorageError)?
        .ok_or_else(|| {
            GuardianError::StorageError("the new reservation is not readable".to_string())
        })?;
    let envelope = ExecutionEnvelope::from_record(&record, true, true);

    let job = ExecutionJob {
        account_id,
        proposal_id,
        attempt,
        nonce: proposal.nonce,
        base_commitment: proposal.prev_commitment,
        scheme: resolved.metadata.auth.scheme(),
        input,
        fence,
        lease,
        elector,
        executor,
    };
    let worker_state = state.clone();
    tokio::spawn(async move { run_execution(&worker_state, job).await });

    Ok(envelope)
}

/// The answer for a request that lost the race for the account's lease: the winner's execution
/// when it is this proposal's, a conflict otherwise.
async fn already_executing(
    state: &AppState,
    account_id: &str,
    proposal_id: String,
) -> Result<ExecutionEnvelope> {
    match state
        .storage
        .load_active_execution(account_id)
        .await
        .map_err(GuardianError::StorageError)?
    {
        Some(active) => answer_active(active, &proposal_id),
        None => Err(GuardianError::ExecutionBusy),
    }
}

/// An account already reserved: this proposal's execution when it is the one running, a
/// conflict naming the blocker otherwise.
fn answer_active(active: ExecutionRecord, proposal_id: &str) -> Result<ExecutionEnvelope> {
    if active.reservation.proposal_id == proposal_id {
        return Ok(ExecutionEnvelope::from_record(&active, true, false));
    }
    Err(GuardianError::ExecutionConflict {
        blocking_proposal_id: active.reservation.proposal_id,
    })
}
