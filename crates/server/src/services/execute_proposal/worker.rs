//! The background half of an execution, run in order under the reservation.
//!
//! Before the boundary commit every failure fails and releases the reservation; from the
//! boundary on the transaction is never proved or sent again, only resolved.

use std::sync::{Arc, Mutex};

use chrono::Utc;
use guardian_shared::SignatureScheme;

use super::executor::{
    ExecutedTransactionInfo, ExecutionAttempt, ExecutionInput, GuardianAck, ProposalExecutor,
    ProvenTransactionInfo, SubmissionOutcome,
};
use crate::coordination::{LeaderElector, Lease, lease_deadline, release_quietly};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::ack_delta_internal::{AcknowledgedDelta, acknowledge_delta};
use crate::state::AppState;
use crate::storage::execution::ExpirationBound;
use crate::storage::{
    AdmissionWrite, CandidateAdmission, ExecutionFailure, ExecutionFailureCode, ExecutionPhase,
    ExecutionResolution, LeaseFence, ReservationUpdate, ResolveWrite, SubmissionEvidence,
};

pub(super) struct ExecutionJob {
    pub account_id: String,
    pub proposal_id: String,
    pub attempt: u32,
    pub nonce: u64,
    pub base_commitment: String,
    pub scheme: SignatureScheme,
    pub input: ExecutionInput,
    pub fence: LeaseFence,
    pub lease: Lease,
    pub elector: Arc<dyn LeaderElector>,
    pub executor: Arc<dyn ProposalExecutor>,
}

/// Why the worker stopped before the boundary.
enum Stop {
    /// The attempt failed; its outcome is written and the reservation released.
    Failed(ExecutionFailure),
    /// The worker no longer owns the reservation and writes nothing.
    OwnershipLost,
}

impl From<ExecutionFailure> for Stop {
    fn from(failure: ExecutionFailure) -> Self {
        Stop::Failed(failure)
    }
}

/// Keeps the worker's lease and reservation alive while it holds the attempt, and records the
/// phase it reached.
struct Heartbeat {
    phase: Arc<Mutex<ExecutionPhase>>,
    task: tokio::task::JoinHandle<()>,
}

/// How soon a heartbeat retries a renewal that failed for a transient reason.
const HEARTBEAT_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

impl Heartbeat {
    fn start(state: &AppState, job: &ExecutionJob) -> Self {
        let phase = Arc::new(Mutex::new(ExecutionPhase::Accepted));
        let interval = state.execution.config.lease / 3;
        let ttl = state.execution.config.lease;
        let storage = state.storage.clone();
        let elector = job.elector.clone();
        let lease = job.lease.clone();
        let fence = job.fence.clone();
        let account_id = job.account_id.clone();
        let current = phase.clone();
        let task = tokio::spawn(async move {
            let mut held_until = lease.expires_at;
            let mut wait = interval;
            loop {
                tokio::time::sleep(wait).await;
                let renewing_until = lease_deadline(ttl);
                match elector.renew(&lease, ttl).await {
                    Ok(true) => held_until = renewing_until,
                    Ok(false) => {
                        tracing::warn!(%account_id, "execution lease lost; the attempt will stop before its next write");
                        return;
                    }
                    Err(error) if Utc::now() < held_until => {
                        tracing::warn!(%account_id, %error, "could not renew the execution lease; retrying while it is still held");
                        wait = HEARTBEAT_RETRY.min(interval);
                        continue;
                    }
                    Err(error) => {
                        tracing::warn!(%account_id, %error, "the execution lease lapsed while it could not be renewed");
                        return;
                    }
                }
                let phase = *current.lock().expect("phase lock");
                let expires = lease_deadline(ttl);
                match storage
                    .renew_execution_reservation(&account_id, &fence, expires, phase)
                    .await
                {
                    Ok(ReservationUpdate::Applied) => wait = interval,
                    Ok(ReservationUpdate::StaleLease | ReservationUpdate::NotActive) => {
                        tracing::warn!(%account_id, "the execution reservation is no longer this attempt's; the attempt will stop before its next write");
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(%account_id, %error, "failed to renew the execution reservation; retrying while the lease is held");
                        wait = HEARTBEAT_RETRY.min(interval);
                    }
                }
            }
        });
        Self { phase, task }
    }

    async fn advance(&self, state: &AppState, job: &ExecutionJob, phase: ExecutionPhase) {
        *self.phase.lock().expect("phase lock") = phase;
        let expires = lease_deadline(state.execution.config.lease);
        if let Err(error) = state
            .storage
            .renew_execution_reservation(&job.account_id, &job.fence, expires, phase)
            .await
        {
            tracing::warn!(account_id = %job.account_id, %error, "failed to record the execution phase");
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn run_execution(state: &AppState, job: ExecutionJob) {
    let heartbeat = Heartbeat::start(state, &job);
    match run_to_boundary(state, &job, &heartbeat).await {
        Ok(mut attempt) => {
            drop(heartbeat);
            if claim_send(state, &job).await {
                submit(state, &job, attempt.as_mut()).await;
            } else {
                tracing::warn!(
                    account_id = %job.account_id,
                    proposal_id = %job.proposal_id,
                    "lost the reservation after the boundary commit; nothing was sent"
                );
            }
        }
        Err(Stop::Failed(failure)) => {
            drop(heartbeat);
            fail(state, &job, failure).await;
        }
        Err(Stop::OwnershipLost) => {
            tracing::warn!(
                account_id = %job.account_id,
                proposal_id = %job.proposal_id,
                "execution stopped after losing its reservation; the new owner resolves it"
            );
        }
    }
}

async fn run_to_boundary(
    state: &AppState,
    job: &ExecutionJob,
    heartbeat: &Heartbeat,
) -> Result<Box<dyn ExecutionAttempt>, Stop> {
    let current_state = state
        .storage
        .pull_state(&job.account_id)
        .await
        .map_err(|e| ExecutionFailure::new(ExecutionFailureCode::StateMismatch, e))?;
    if current_state.commitment != job.base_commitment {
        return Err(ExecutionFailure::new(
            ExecutionFailureCode::StateMismatch,
            "the account advanced past the proposal's base state",
        )
        .into());
    }

    let mut attempt = job.executor.prepare(job.input.clone()).await?;
    heartbeat
        .advance(state, job, ExecutionPhase::Verified)
        .await;

    let acknowledged = acknowledge(state, job, &current_state, attempt.as_ref()).await?;
    heartbeat
        .advance(state, job, ExecutionPhase::Acknowledged)
        .await;
    let ack = attempt.requires_guardian_ack().then(|| GuardianAck {
        scheme: job.scheme,
        signature_hex: acknowledged.delta.ack_sig.clone(),
        public_key_hex: acknowledged.delta.ack_pubkey.clone(),
        commitment_hex: state.ack.commitment(&job.scheme),
    });

    let executed = attempt.execute(ack).await?;
    if !executed
        .final_account_commitment
        .eq_ignore_ascii_case(&acknowledged.applied.commitment)
    {
        return Err(ExecutionFailure::new(
            ExecutionFailureCode::BindingMismatch,
            "the executed account state differs from the acknowledged one",
        )
        .into());
    }
    ensure_within_horizon(state, &executed)?;
    heartbeat
        .advance(state, job, ExecutionPhase::Executed)
        .await;
    heartbeat.advance(state, job, ExecutionPhase::Proving).await;

    let proven = attempt.prove().await?;
    heartbeat.advance(state, job, ExecutionPhase::Proved).await;
    attempt.seal().await?;
    ensure_admissible(state, job).await?;
    ensure_unexpired(job, &proven).await?;

    cross_boundary(state, job, acknowledged, &proven).await?;
    Ok(attempt)
}

async fn acknowledge(
    state: &AppState,
    job: &ExecutionJob,
    current_state: &crate::state_object::StateObject,
    attempt: &dyn ExecutionAttempt,
) -> Result<AcknowledgedDelta, Stop> {
    let delta = DeltaObject {
        account_id: job.account_id.clone(),
        nonce: job.nonce,
        prev_commitment: current_state.commitment.clone(),
        new_commitment: None,
        delta_payload: attempt.summary_payload().clone(),
        ack_sig: String::new(),
        ack_pubkey: String::new(),
        ack_scheme: String::new(),
        status: DeltaStatus::candidate(Utc::now().to_rfc3339()),
        metadata: None,
    };
    acknowledge_delta(state, &job.scheme, current_state, &delta)
        .await
        .map_err(|error| {
            ExecutionFailure::new(
                ExecutionFailureCode::BindingMismatch,
                format!("Guardian could not acknowledge the reproduced delta: {error}"),
            )
            .into()
        })
}

async fn ensure_admissible(state: &AppState, job: &ExecutionJob) -> Result<(), Stop> {
    let inadmissible = |message: String| {
        Stop::Failed(ExecutionFailure::new(
            ExecutionFailureCode::AccountInadmissible,
            message,
        ))
    };
    let current_state = state
        .storage
        .pull_state(&job.account_id)
        .await
        .map_err(inadmissible)?;
    if current_state.commitment != job.base_commitment {
        return Err(inadmissible(
            "the account moved during the attempt".to_string(),
        ));
    }
    let metadata = state
        .metadata
        .get(&job.account_id)
        .await
        .map_err(inadmissible)?
        .ok_or_else(|| inadmissible("the account metadata disappeared".to_string()))?;
    ensure_account_active_metadata(&metadata).map_err(|e| inadmissible(e.to_string()))?;
    if let Some(guardian) = state
        .network_client
        .extract_guardian_commitment(&current_state.state_json)
        .map_err(inadmissible)?
        && !guardian.eq_ignore_ascii_case(&state.ack.commitment(&job.scheme))
    {
        return Err(inadmissible(
            "the account is no longer guarded by this Guardian".to_string(),
        ));
    }
    Ok(())
}

/// Checked as soon as execution fixes the expiration, so a transaction this server could not
/// resolve is refused before any proving is spent on it.
fn ensure_within_horizon(state: &AppState, executed: &ExecutedTransactionInfo) -> Result<(), Stop> {
    let distance = executed
        .expiration_block
        .saturating_sub(executed.reference_block);
    if distance > state.execution.config.expiration_horizon_blocks {
        return Err(ExecutionFailure::new(
            ExecutionFailureCode::ExpirationBeyondHorizon,
            format!(
                "the transaction expires {distance} blocks after its reference block; this \
                 server resolves at most {}",
                state.execution.config.expiration_horizon_blocks
            ),
        )
        .into());
    }
    Ok(())
}

/// Proving can take minutes, so the tip is read again just before the boundary: a transaction
/// that can no longer be included is refused while its proposal can still be retried, rather
/// than sent and settled as expired with its proposal gone.
async fn ensure_unexpired(job: &ExecutionJob, proven: &ProvenTransactionInfo) -> Result<(), Stop> {
    let tip =
        job.executor.chain_tip().await.map_err(|message| {
            ExecutionFailure::new(ExecutionFailureCode::NodeUnavailable, message)
        })?;
    if proven.expiration_block <= tip {
        return Err(ExecutionFailure::new(
            ExecutionFailureCode::ExpirationReached(ExpirationBound::Transaction),
            format!(
                "the transaction expires at block {}; the tip is {tip}",
                proven.expiration_block
            ),
        )
        .into());
    }
    Ok(())
}

/// The no-retry boundary: the candidate and the evidence commit together, or not at all.
async fn cross_boundary(
    state: &AppState,
    job: &ExecutionJob,
    acknowledged: AcknowledgedDelta,
    proven: &ProvenTransactionInfo,
) -> Result<(), Stop> {
    let now = Utc::now();
    let mut candidate = acknowledged.delta;
    candidate.status = DeltaStatus::candidate(now.to_rfc3339());
    let admission = CandidateAdmission {
        fence: job.fence.clone(),
        evidence: SubmissionEvidence {
            account_id: job.account_id.clone(),
            proposal_id: job.proposal_id.clone(),
            attempt: job.attempt,
            candidate_nonce: candidate.nonce,
            transaction_id: proven.transaction_id.clone(),
            expected_commitment: acknowledged.applied.commitment,
            reference_block: proven.reference_block,
            expiration_block: proven.expiration_block,
            base_commitment: job.base_commitment.clone(),
            committed_at: now,
        },
        delta: candidate,
        now,
    };
    let outcome = state
        .storage
        .admit_execution_candidate(state.metadata.as_ref(), admission)
        .await
        .map_err(|e| ExecutionFailure::new(ExecutionFailureCode::StateMismatch, e))?;
    match outcome {
        AdmissionWrite::Admitted => Ok(()),
        AdmissionWrite::NotAuthorized | AdmissionWrite::StaleLease => Err(Stop::OwnershipLost),
        AdmissionWrite::StaleBase => Err(ExecutionFailure::new(
            ExecutionFailureCode::AccountInadmissible,
            "the account moved before the boundary commit",
        )
        .into()),
        AdmissionWrite::AccountInactive => Err(ExecutionFailure::new(
            ExecutionFailureCode::AccountInadmissible,
            "the account was paused or released before the boundary commit",
        )
        .into()),
        AdmissionWrite::CandidateExists | AdmissionWrite::NonceOccupied => {
            Err(ExecutionFailure::new(
                ExecutionFailureCode::StateMismatch,
                "another delta took the account's next nonce",
            )
            .into())
        }
    }
}

/// The boundary commit's own fence check is not enough: a worker can pause between the commit
/// and the send while its lease expires and another owner takes over. The fenced renewal that
/// records the send is the last write before it, so it is the gate:
/// only a worker whose fence still matches the persisted reservation sends. A storage error
/// leaves ownership unknown, and an unsent transaction is safe where a second owner's is not.
async fn claim_send(state: &AppState, job: &ExecutionJob) -> bool {
    if !job.elector.verify_held(&job.lease).await.unwrap_or(false) {
        return false;
    }
    let expires = lease_deadline(state.execution.config.lease);
    match state
        .storage
        .renew_execution_reservation(&job.account_id, &job.fence, expires, ExecutionPhase::Sent)
        .await
    {
        Ok(ReservationUpdate::Applied) => true,
        Ok(ReservationUpdate::StaleLease | ReservationUpdate::NotActive) => false,
        Err(error) => {
            tracing::warn!(account_id = %job.account_id, %error, "could not record the send; not sending");
            false
        }
    }
}

async fn submit(state: &AppState, job: &ExecutionJob, attempt: &mut dyn ExecutionAttempt) {
    match attempt.submit().await {
        SubmissionOutcome::Accepted => {
            tracing::info!(account_id = %job.account_id, proposal_id = %job.proposal_id, "Guardian execution submitted");
        }
        SubmissionOutcome::Unknown { reason } => {
            tracing::warn!(
                account_id = %job.account_id,
                proposal_id = %job.proposal_id,
                %reason,
                "submission outcome unknown; the reservation is held until the chain resolves it"
            );
        }
        SubmissionOutcome::Rejected { reason } => {
            let resolution = resolution(
                job,
                ExecutionFailure::new(ExecutionFailureCode::SubmissionRejected, reason),
            );
            match state
                .storage
                .resolve_execution(state.metadata.as_ref(), resolution)
                .await
            {
                Ok(ResolveWrite::Resolved) => crate::metrics::execution::record_outcome(Some(
                    &ExecutionFailureCode::SubmissionRejected,
                )),
                Ok(ResolveWrite::AlreadyResolved) => {}
                Ok(other) => {
                    tracing::warn!(account_id = %job.account_id, ?other, "could not resolve a rejected submission; reconciliation will")
                }
                Err(error) => {
                    tracing::error!(account_id = %job.account_id, %error, "failed to resolve a rejected submission")
                }
            }
        }
    }
}

async fn fail(state: &AppState, job: &ExecutionJob, failure: ExecutionFailure) {
    tracing::info!(
        account_id = %job.account_id,
        proposal_id = %job.proposal_id,
        code = failure.code.as_str(),
        message = %failure.message,
        "Guardian execution failed before submission"
    );
    let code = failure.code;
    match state.storage.fail_execution(resolution(job, failure)).await {
        Ok(ResolveWrite::Resolved) => crate::metrics::execution::record_outcome(Some(&code)),
        Ok(ResolveWrite::AlreadyResolved) => {}
        Ok(other) => {
            tracing::warn!(account_id = %job.account_id, ?other, "the failed execution was not released by this worker")
        }
        Err(error) => {
            tracing::error!(account_id = %job.account_id, %error, "failed to record a failed execution")
        }
    }
    release_quietly(job.elector.as_ref(), job.lease.clone()).await;
}

fn resolution(job: &ExecutionJob, failure: ExecutionFailure) -> ExecutionResolution {
    ExecutionResolution {
        account_id: job.account_id.clone(),
        proposal_id: job.proposal_id.clone(),
        attempt: job.attempt,
        fence: job.fence.clone(),
        failure,
        now: Utc::now(),
    }
}
