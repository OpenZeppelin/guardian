//! The background half of an execution, run in order under the reservation.
//!
//! Before the boundary commit every failure fails and releases the reservation; from the
//! boundary on the transaction is never proved or sent again, only resolved.

use std::sync::Arc;

use chrono::Utc;
use guardian_shared::SignatureScheme;

use super::executor::{
    ExecutedTransactionInfo, ExecutionAttempt, ExecutionInput, GuardianAck, ProposalExecutor,
    ProvenTransactionInfo, SubmissionOutcome,
};
use crate::coordination::{LeaderElector, Lease, Renewal, release_quietly};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::GuardianError;
use crate::services::account_status::ensure_account_active_metadata;
use crate::services::ack_delta_internal::{AcknowledgedDelta, acknowledge_delta};
use crate::state::AppState;
use crate::storage::execution::ExpirationBound;
use crate::storage::{
    AdmissionWrite, CandidateAdmission, ExecutionFailure, ExecutionFailureCode, ExecutionPhase,
    ExecutionResolution, LeaseFence, ResolveWrite, SubmissionEvidence,
};

pub(super) struct ExecutionJob {
    pub account_id: String,
    pub proposal_id: String,
    pub attempt: u32,
    pub nonce: u64,
    pub base_commitment: String,
    pub scheme: SignatureScheme,
    pub fence: LeaseFence,
    pub lease: Lease,
    pub elector: Arc<dyn LeaderElector>,
    pub executor: Arc<dyn ProposalExecutor>,
    /// Counts this execution towards the server's cap until the worker returns.
    pub permit: tokio::sync::OwnedSemaphorePermit,
}

impl ExecutionJob {
    /// Renews this attempt's lease, then its reservation, recording `phase`.
    async fn renew(&self, state: &AppState, phase: ExecutionPhase) -> Renewal {
        Renewal::attempt(
            self.elector.as_ref(),
            &self.lease,
            &self.fence,
            state.storage.as_ref(),
            &self.account_id,
            state.execution.config.lease,
            phase,
        )
        .await
    }
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
/// phase it reached. The phase lock is held across each renewal and its write, so a heartbeat
/// can never persist an older phase or deadline after a newer one.
struct Heartbeat {
    phase: Arc<tokio::sync::Mutex<ExecutionPhase>>,
    task: tokio::task::JoinHandle<()>,
}

/// How soon a heartbeat retries a renewal that failed for a transient reason.
const HEARTBEAT_RETRY: std::time::Duration = std::time::Duration::from_secs(1);

impl Heartbeat {
    fn start(state: &AppState, job: &ExecutionJob) -> Self {
        let phase = Arc::new(tokio::sync::Mutex::new(ExecutionPhase::Accepted));
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
                let phase = current.lock().await;
                match Renewal::attempt(
                    elector.as_ref(),
                    &lease,
                    &fence,
                    storage.as_ref(),
                    &account_id,
                    ttl,
                    *phase,
                )
                .await
                {
                    Renewal::Held(renewed_until) => {
                        held_until = renewed_until;
                        wait = interval;
                    }
                    Renewal::LeaseLost => {
                        tracing::warn!(%account_id, "execution lease lost; the attempt will stop before its next write");
                        return;
                    }
                    Renewal::LeaseUnavailable(error) if Utc::now() < held_until => {
                        tracing::warn!(%account_id, %error, "could not renew the execution lease; retrying while it is still held");
                        wait = HEARTBEAT_RETRY.min(interval);
                    }
                    Renewal::LeaseUnavailable(error) => {
                        tracing::warn!(%account_id, %error, "the execution lease lapsed while it could not be renewed");
                        return;
                    }
                    Renewal::ReservationLost => {
                        tracing::warn!(%account_id, "the execution reservation is no longer this attempt's; the attempt will stop before its next write");
                        return;
                    }
                    Renewal::ReservationUnavailable {
                        renewed_until,
                        error,
                    } => {
                        held_until = renewed_until;
                        tracing::warn!(%account_id, %error, "failed to renew the execution reservation; retrying while the lease is held");
                        wait = HEARTBEAT_RETRY.min(interval);
                    }
                }
            }
        });
        Self { phase, task }
    }

    /// Records the phase under a renewed lease.
    async fn advance(
        &self,
        state: &AppState,
        job: &ExecutionJob,
        phase: ExecutionPhase,
    ) -> Result<(), Stop> {
        let mut recorded = self.phase.lock().await;
        *recorded = phase;
        match job.renew(state, phase).await {
            Renewal::Held(_) => Ok(()),
            Renewal::LeaseLost | Renewal::ReservationLost => Err(Stop::OwnershipLost),
            Renewal::LeaseUnavailable(error) => {
                tracing::warn!(account_id = %job.account_id, %error, "could not renew the execution lease to record the phase; the heartbeat keeps retrying");
                Ok(())
            }
            Renewal::ReservationUnavailable { error, .. } => {
                tracing::warn!(account_id = %job.account_id, %error, "failed to record the execution phase");
                Ok(())
            }
        }
    }

    /// Stops renewing and waits for an in-flight renewal to finish, so none can land after a
    /// later write such as the send claim.
    async fn stop(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Wall-clock time of each worker phase, reported on the line that records how the execution
/// ended, so one log line shows where an execution spent its time.
struct PhaseTimings {
    started: std::time::Instant,
    last: std::time::Instant,
    phases: Vec<(&'static str, std::time::Duration)>,
}

impl PhaseTimings {
    fn start() -> Self {
        let now = std::time::Instant::now();
        Self {
            started: now,
            last: now,
            phases: Vec::new(),
        }
    }

    /// Closes `phase`, which ran since the previous mark, and records it.
    fn mark(&mut self, phase: &'static str) {
        let now = std::time::Instant::now();
        let took = now - self.last;
        crate::metrics::execution::record_phase(phase, took);
        self.phases.push((phase, took));
        self.last = now;
    }

    fn total_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }
}

impl std::fmt::Display for PhaseTimings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, (phase, took)) in self.phases.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{phase}={}ms", took.as_millis())?;
        }
        Ok(())
    }
}

/// The worker gives its lease up on every exit. After the send the reservation stays held under
/// the worker's fence and reconciliation claims it, so a request arriving meanwhile reads the
/// execution in flight instead of being turned away as busy.
pub(super) async fn run_execution(state: &AppState, job: ExecutionJob, input: ExecutionInput) {
    let mut timings = PhaseTimings::start();
    let heartbeat = Heartbeat::start(state, &job);
    let reached = run_to_boundary(state, &job, input, &heartbeat, &mut timings).await;
    heartbeat.stop().await;
    match reached {
        Ok(mut attempt) => {
            if claim_send(state, &job).await {
                submit(state, &job, attempt.as_mut(), &mut timings).await;
            } else {
                tracing::warn!(
                    account_id = %job.account_id,
                    proposal_id = %job.proposal_id,
                    phases = %timings,
                    total_ms = timings.total_ms(),
                    "lost the reservation after the boundary commit; nothing was sent"
                );
            }
        }
        Err(Stop::Failed(failure)) => fail(state, &job, failure, &timings).await,
        Err(Stop::OwnershipLost) => {
            tracing::warn!(
                account_id = %job.account_id,
                proposal_id = %job.proposal_id,
                phases = %timings,
                total_ms = timings.total_ms(),
                "execution stopped after losing its reservation; the new owner resolves it"
            );
        }
    }
    release_quietly(job.elector.as_ref(), job.lease.clone()).await;
    drop(job.permit);
}

/// A shutdown stops the attempt anywhere short of the boundary commit, never inside it: the work
/// before it only reads, signs and proves, so dropping it leaves nothing behind but the
/// reservation, which the failure then releases.
async fn run_to_boundary(
    state: &AppState,
    job: &ExecutionJob,
    input: ExecutionInput,
    heartbeat: &Heartbeat,
    timings: &mut PhaseTimings,
) -> Result<Box<dyn ExecutionAttempt>, Stop> {
    let (attempt, acknowledged, proven) = tokio::select! {
        biased;
        _ = state.execution.shutdown.cancelled() => {
            return Err(ExecutionFailure::new(
                ExecutionFailureCode::Abandoned,
                "the server shut down before the attempt reached the no-retry boundary; \
                 request execution again",
            )
            .into());
        }
        reached = approach_boundary(state, job, input, heartbeat, timings) => reached?,
    };
    cross_boundary(state, job, acknowledged, &proven).await?;
    timings.mark("boundary");
    Ok(attempt)
}

async fn approach_boundary(
    state: &AppState,
    job: &ExecutionJob,
    input: ExecutionInput,
    heartbeat: &Heartbeat,
    timings: &mut PhaseTimings,
) -> Result<
    (
        Box<dyn ExecutionAttempt>,
        AcknowledgedDelta,
        ProvenTransactionInfo,
    ),
    Stop,
> {
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

    let mut attempt = job.executor.prepare(input).await?;
    heartbeat
        .advance(state, job, ExecutionPhase::Verified)
        .await?;
    timings.mark("prepare");

    let acknowledged = acknowledge(state, job, &current_state, attempt.as_ref()).await?;
    heartbeat
        .advance(state, job, ExecutionPhase::Acknowledged)
        .await?;
    timings.mark("acknowledge");
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
        .advance(state, job, ExecutionPhase::Proving)
        .await?;
    timings.mark("execute");

    let proven = attempt.prove().await?;
    heartbeat
        .advance(state, job, ExecutionPhase::Proved)
        .await?;
    timings.mark("prove");
    attempt.seal().await?;
    timings.mark("seal");
    ensure_admissible(state, job).await?;
    ensure_unexpired(job, &proven).await?;
    timings.mark("checks");
    Ok((attempt, acknowledged, proven))
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
    acknowledge_delta(
        state,
        &job.scheme,
        &current_state.commitment,
        &current_state.state_json,
        &delta,
    )
    .await
    .map_err(|error| {
        let code = match error {
            GuardianError::SigningError(_)
            | GuardianError::StorageError(_)
            | GuardianError::ConfigurationError(_) => ExecutionFailureCode::AcknowledgementFailed,
            _ => ExecutionFailureCode::BindingMismatch,
        };
        ExecutionFailure::new(
            code,
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
    match job.renew(state, ExecutionPhase::Sent).await {
        Renewal::Held(_) => true,
        Renewal::LeaseLost | Renewal::LeaseUnavailable(_) | Renewal::ReservationLost => false,
        Renewal::ReservationUnavailable { error, .. } => {
            tracing::warn!(account_id = %job.account_id, %error, "could not record the send; not sending");
            false
        }
    }
}

async fn submit(
    state: &AppState,
    job: &ExecutionJob,
    attempt: &mut dyn ExecutionAttempt,
    timings: &mut PhaseTimings,
) {
    let outcome = attempt.submit().await;
    timings.mark("send");
    match outcome {
        SubmissionOutcome::Accepted => {
            tracing::info!(
                account_id = %job.account_id,
                proposal_id = %job.proposal_id,
                phases = %timings,
                total_ms = timings.total_ms(),
                "Guardian execution submitted"
            );
        }
        SubmissionOutcome::Unknown { reason } => {
            tracing::warn!(
                account_id = %job.account_id,
                proposal_id = %job.proposal_id,
                %reason,
                phases = %timings,
                total_ms = timings.total_ms(),
                "submission outcome unknown; the reservation is held until the chain resolves it"
            );
        }
        SubmissionOutcome::Rejected { reason } => {
            tracing::warn!(
                account_id = %job.account_id,
                proposal_id = %job.proposal_id,
                %reason,
                phases = %timings,
                total_ms = timings.total_ms(),
                "the node rejected the submission"
            );
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

async fn fail(
    state: &AppState,
    job: &ExecutionJob,
    failure: ExecutionFailure,
    timings: &PhaseTimings,
) {
    tracing::info!(
        account_id = %job.account_id,
        proposal_id = %job.proposal_id,
        code = failure.code.as_str(),
        message = %failure.message,
        phases = %timings,
        total_ms = timings.total_ms(),
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

#[cfg(test)]
mod phase_timing_tests {
    use super::PhaseTimings;

    #[test]
    fn every_closed_phase_is_reported_in_order_with_its_duration() {
        let mut timings = PhaseTimings::start();
        timings.mark("prepare");
        timings.mark("prove");
        let rendered = timings.to_string();
        let phases: Vec<&str> = rendered
            .split(' ')
            .map(|phase| phase.split('=').next().unwrap())
            .collect();
        assert_eq!(phases, ["prepare", "prove"]);
        assert!(
            rendered.split(' ').all(|phase| phase.ends_with("ms")),
            "{rendered}"
        );
    }
}
