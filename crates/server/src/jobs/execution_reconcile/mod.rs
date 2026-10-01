//! Resolves executions whose worker is gone. Before the no-retry boundary an expired lease
//! fails and releases the attempt; after it, ownership moves here and the attempt is settled
//! only from what the chain shows, never from elapsed time. `committed` is never written here:
//! promotion writes it.

use std::sync::Arc;

use chrono::Utc;

use crate::coordination::{LeaderElector, Lease, lease_deadline, release_quietly};
use crate::network::{RpcReadMode, StateVerification};
use crate::services::execute_proposal::ProposalExecutor;
use crate::state::AppState;
use crate::storage::{
    ClaimWrite, ExecutionFailure, ExecutionFailureCode, ExecutionPhase, ExecutionRecord,
    ExecutionResolution, LeaseFence, ResolveWrite, SubmissionEvidence,
};

/// Which pass found an expired pre-boundary attempt. The first pass after a start reports
/// attempts interrupted by the restart as abandoned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassKind {
    Startup,
    Steady,
}

/// What one pass did with one execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciled {
    /// A live worker still owns the attempt, or another reconciler does.
    Owned,
    /// The attempt failed before the boundary and was released.
    Released(ExecutionFailureCode),
    /// The account is at the expected state; promotion settles it.
    AwaitingPromotion,
    /// Nothing the chain shows settles it yet.
    Waiting,
    /// The chain could not be observed; the attempt stays held.
    ObservationUnavailable,
    /// The submission can no longer land and was resolved failed.
    Resolved(ExecutionFailureCode),
}

impl Reconciled {
    fn label(self) -> &'static str {
        match self {
            Reconciled::Owned => "owned",
            Reconciled::Released(_) => "released",
            Reconciled::AwaitingPromotion => "awaiting_promotion",
            Reconciled::Waiting => "waiting",
            Reconciled::ObservationUnavailable => "observation_unavailable",
            Reconciled::Resolved(_) => "resolved",
        }
    }
}

pub fn start_execution_reconciler(state: AppState, executor: Arc<dyn ProposalExecutor>) {
    tokio::spawn(async move {
        let reconciler = Reconciler::new(&state, executor);
        let mut kind = PassKind::Startup;
        loop {
            if let Err(error) = reconciler.pass(&state, kind).await {
                tracing::warn!(%error, "execution reconciliation pass failed");
            }
            kind = PassKind::Steady;
            tokio::time::sleep(state.execution.config.reconcile_interval).await;
        }
    });
}

pub struct Reconciler {
    holder_id: String,
    executor: Arc<dyn ProposalExecutor>,
    /// When the current run of passes that could not observe the chain began.
    outage_since: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Reconciler {
    pub fn new(state: &AppState, executor: Arc<dyn ProposalExecutor>) -> Self {
        Self {
            holder_id: format!("{}:reconcile", state.execution.replica_id),
            executor,
            outage_since: std::sync::Mutex::new(None),
        }
    }

    pub async fn pass(
        &self,
        state: &AppState,
        kind: PassKind,
    ) -> Result<Vec<(ExecutionRecord, Reconciled)>, String> {
        let mut report = Vec::new();
        let records = state.storage.list_active_executions().await?;
        let now = Utc::now();
        crate::metrics::execution::record_oldest_reservation_age(
            records
                .iter()
                .map(|record| {
                    (now - record.reservation.created_at)
                        .num_milliseconds()
                        .max(0) as f64
                        / 1000.0
                })
                .fold(0.0, f64::max),
        );
        let tip = tokio::sync::OnceCell::new();
        for record in records {
            let outcome = self.reconcile(state, &record, kind, &tip).await;
            metrics::counter!(
                crate::metrics::names::EXECUTION_RECONCILE_OUTCOMES_TOTAL,
                crate::metrics::names::LABEL_OUTCOME => outcome.label()
            )
            .increment(1);
            report.push((record, outcome));
        }
        self.record_outage(&report);
        Ok(report)
    }

    fn record_outage(&self, report: &[(ExecutionRecord, Reconciled)]) {
        let unobservable = report
            .iter()
            .any(|(_, outcome)| *outcome == Reconciled::ObservationUnavailable);
        let mut since = self.outage_since.lock().expect("outage clock");
        let seconds = match (unobservable, *since) {
            (true, Some(started)) => started.elapsed().as_secs_f64(),
            (true, None) => {
                *since = Some(std::time::Instant::now());
                0.0
            }
            (false, _) => {
                *since = None;
                0.0
            }
        };
        crate::metrics::execution::record_observation_outage(seconds);
    }

    /// `tip` is the chain tip for this pass, read once and only if a record needs it.
    async fn reconcile(
        &self,
        state: &AppState,
        record: &ExecutionRecord,
        kind: PassKind,
        tip: &tokio::sync::OnceCell<u32>,
    ) -> Reconciled {
        let reservation = &record.reservation;
        let ours = reservation.fence.holder_id == self.holder_id;
        if !ours && reservation.lease_expires_at > Utc::now() {
            return Reconciled::Owned;
        }
        let elector = state
            .execution
            .leases
            .elector(&reservation.account_id, &self.holder_id);
        let Some(lease) = self.take_ownership(state, record, elector.as_ref()).await else {
            return Reconciled::Owned;
        };
        let fence = LeaseFence::from(&lease);

        let Some(evidence) = &record.evidence else {
            let code = match kind {
                PassKind::Startup => ExecutionFailureCode::Abandoned,
                PassKind::Steady => ExecutionFailureCode::LeaseExpired,
            };
            let failure = ExecutionFailure {
                code,
                message: "the attempt stopped renewing its lease before the no-retry boundary"
                    .to_string(),
            };
            let written = state
                .storage
                .fail_execution(resolution(record, &fence, failure))
                .await;
            release_quietly(elector.as_ref(), lease).await;
            return match written {
                Ok(ResolveWrite::Resolved) => {
                    crate::metrics::execution::record_outcome(Some(&code));
                    Reconciled::Released(code)
                }
                Ok(_) | Err(_) => Reconciled::Owned,
            };
        };

        let settled = match self.observe(state, evidence, tip).await {
            Ok(settled) => settled,
            Err(error) => {
                tracing::warn!(
                    account_id = %reservation.account_id,
                    proposal_id = %reservation.proposal_id,
                    %error,
                    "cannot observe the chain for a submitted execution; it stays submitted"
                );
                self.hold(state, record, &fence).await;
                return Reconciled::ObservationUnavailable;
            }
        };
        match settled {
            Observed::AtExpected => {
                self.hold(state, record, &fence).await;
                Reconciled::AwaitingPromotion
            }
            Observed::Pending => {
                self.hold(state, record, &fence).await;
                Reconciled::Waiting
            }
            Observed::Settles(code, message) => {
                let failure = ExecutionFailure { code, message };
                match state
                    .storage
                    .resolve_execution(state.metadata.as_ref(), resolution(record, &fence, failure))
                    .await
                {
                    Ok(ResolveWrite::Resolved) => {
                        crate::metrics::execution::record_outcome(Some(&code));
                        release_quietly(elector.as_ref(), lease).await;
                        Reconciled::Resolved(code)
                    }
                    Ok(other) => {
                        tracing::warn!(account_id = %reservation.account_id, ?other, "a settled execution was not resolved");
                        Reconciled::Owned
                    }
                    Err(error) => {
                        tracing::error!(account_id = %reservation.account_id, %error, "failed to resolve a settled execution");
                        Reconciled::Owned
                    }
                }
            }
        }
    }

    /// Takes the account's execution lease and, unless this reconciler already holds the
    /// reservation, moves it here by compare-and-set on the fence the record carries.
    async fn take_ownership(
        &self,
        state: &AppState,
        record: &ExecutionRecord,
        elector: &dyn LeaderElector,
    ) -> Option<Lease> {
        let lease = elector
            .try_acquire(state.execution.config.lease)
            .await
            .ok()
            .flatten()?;
        let fence = LeaseFence::from(&lease);
        if record.reservation.fence == fence {
            return Some(lease);
        }
        match state
            .storage
            .claim_execution_reservation(
                &record.reservation.account_id,
                &record.reservation.fence,
                &fence,
                lease.expires_at,
            )
            .await
        {
            Ok(ClaimWrite::Claimed) => Some(lease),
            Ok(ClaimWrite::ClaimSuperseded | ClaimWrite::StaleLease | ClaimWrite::NotActive)
            | Err(_) => {
                release_quietly(elector, lease).await;
                None
            }
        }
    }

    async fn observe(
        &self,
        state: &AppState,
        evidence: &SubmissionEvidence,
        tip: &tokio::sync::OnceCell<u32>,
    ) -> Result<Observed, String> {
        let verification = state
            .network_client
            .verify_commitment(
                &evidence.account_id,
                &evidence.expected_commitment,
                RpcReadMode::SingleAttempt,
            )
            .await?;
        let at_base = match verification {
            StateVerification::Match => return Ok(Observed::AtExpected),
            StateVerification::Absent => true,
            StateVerification::Mismatch { on_chain } => {
                on_chain.eq_ignore_ascii_case(&evidence.base_commitment)
            }
        };
        if !at_base {
            return Ok(Observed::Settles(
                ExecutionFailureCode::CandidateDiscarded,
                "the account moved to a state that is neither the base nor the submitted one"
                    .to_string(),
            ));
        }
        let tip = *tip.get_or_try_init(|| self.executor.chain_tip()).await?;
        if tip > evidence.expiration_block {
            return Ok(Observed::Settles(
                ExecutionFailureCode::Expired,
                format!(
                    "the chain is at block {tip}, past the transaction's expiration at {}, with the account still at its base",
                    evidence.expiration_block
                ),
            ));
        }
        Ok(Observed::Pending)
    }

    async fn hold(&self, state: &AppState, record: &ExecutionRecord, fence: &LeaseFence) {
        let expires = lease_deadline(state.execution.config.lease);
        if let Err(error) = state
            .storage
            .renew_execution_reservation(
                &record.reservation.account_id,
                fence,
                expires,
                ExecutionPhase::Reconciling,
            )
            .await
        {
            tracing::warn!(account_id = %record.reservation.account_id, %error, "failed to renew a reconciled reservation");
        }
    }
}

enum Observed {
    AtExpected,
    Pending,
    Settles(ExecutionFailureCode, String),
}

fn resolution(
    record: &ExecutionRecord,
    fence: &LeaseFence,
    failure: ExecutionFailure,
) -> ExecutionResolution {
    ExecutionResolution {
        account_id: record.reservation.account_id.clone(),
        proposal_id: record.reservation.proposal_id.clone(),
        attempt: record.reservation.attempt,
        fence: fence.clone(),
        failure,
        now: Utc::now(),
    }
}

#[cfg(test)]
mod tests;
