//! Resolves executions whose worker is gone. Before the no-retry boundary an expired lease
//! fails and releases the attempt; after it, ownership moves here and the attempt is settled
//! only from what the chain shows, never from elapsed time. `committed` is never written here:
//! promotion writes it.

use chrono::Utc;

use crate::coordination::{LeaderElector, Lease, release_quietly};
use crate::network::{RpcReadMode, StateVerification};
use crate::state::AppState;
use crate::storage::{
    ClaimWrite, ExecutionFailure, ExecutionFailureCode, ExecutionPhase, ExecutionRecord,
    ExecutionResolution, LeaseFence, ReservationUpdate, ResolveWrite, SettleWrite,
    SubmissionEvidence,
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
    /// Promotion had already made the candidate canonical without settling the execution, so
    /// reconciliation recorded it committed.
    Committed,
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
            Reconciled::Committed => "committed",
            Reconciled::Waiting => "waiting",
            Reconciled::ObservationUnavailable => "observation_unavailable",
            Reconciled::Resolved(_) => "resolved",
        }
    }
}

/// Runs whenever canonicalization does, whether or not this server offers execution: switching
/// execution off must not strand the accounts whose executions are already in flight.
pub fn start_execution_reconciler(state: AppState) {
    tokio::spawn(async move {
        let reconciler = Reconciler::new(&state);
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
    /// When the current run of passes that could not observe the chain began.
    outage_since: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Reconciler {
    pub fn new(state: &AppState) -> Self {
        Self {
            holder_id: format!("{}:reconcile", state.execution.replica_id),
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
        for record in records {
            let outcome = self.reconcile(state, &record, kind).await;
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

    async fn reconcile(
        &self,
        state: &AppState,
        record: &ExecutionRecord,
        kind: PassKind,
    ) -> Reconciled {
        let reservation = &record.reservation;
        let ours = reservation.fence.holder_id == self.holder_id;
        // Process-local leases do not survive a restart, so on the first pass every reservation
        // they still name belongs to the process that stopped, however long its lease had left.
        let holder_restarted = kind == PassKind::Startup && !state.execution.leases.is_shared();
        if !ours && !holder_restarted && reservation.lease_expires_at > Utc::now() {
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

        let settled = match self.observe(state, evidence).await {
            Ok(settled) => settled,
            Err(error) => {
                tracing::warn!(
                    account_id = %reservation.account_id,
                    proposal_id = %reservation.proposal_id,
                    %error,
                    "cannot observe the chain for a submitted execution; it stays submitted"
                );
                self.hold(state, record, elector.as_ref(), lease).await;
                return Reconciled::ObservationUnavailable;
            }
        };
        match settled {
            Observed::AtExpected => {
                match state
                    .storage
                    .settle_promoted_execution(&reservation.account_id, &fence, Utc::now())
                    .await
                {
                    Ok(SettleWrite::Settled) => {
                        crate::metrics::execution::record_outcome(None);
                        release_quietly(elector.as_ref(), lease).await;
                        Reconciled::Committed
                    }
                    Ok(
                        SettleWrite::NotPromoted | SettleWrite::StaleLease | SettleWrite::NotActive,
                    )
                    | Err(_) => {
                        self.hold(state, record, elector.as_ref(), lease).await;
                        Reconciled::AwaitingPromotion
                    }
                }
            }
            Observed::Pending => {
                self.hold(state, record, elector.as_ref(), lease).await;
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
        let lease = match elector.try_acquire(state.execution.config.lease).await {
            Ok(lease) => lease?,
            Err(error) => {
                tracing::warn!(account_id = %record.reservation.account_id, %error, "could not take the execution lease to reconcile");
                return None;
            }
        };
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

    /// The verdict comes from one account read and the block the node read it at. A transaction
    /// expiring at block `X` can be included in block `X` and no later, so an account still at
    /// its base as of `X` was not included and can no longer be. The base was on chain at the
    /// block the transaction executed against, so a read from before that block is a lagging
    /// node, not evidence the account moved.
    async fn observe(
        &self,
        state: &AppState,
        evidence: &SubmissionEvidence,
    ) -> Result<Observed, String> {
        let observed = state
            .network_client
            .observe_commitment(
                &evidence.account_id,
                &evidence.expected_commitment,
                RpcReadMode::SingleAttempt,
            )
            .await?;
        let at_base = match observed.verification {
            StateVerification::Match => return Ok(Observed::AtExpected),
            StateVerification::Absent => true,
            StateVerification::Mismatch { on_chain } => {
                on_chain.eq_ignore_ascii_case(&evidence.base_commitment)
            }
        };
        if !at_base {
            if observed.block < evidence.reference_block {
                return Ok(Observed::Pending);
            }
            return Ok(Observed::Settles(
                ExecutionFailureCode::CandidateDiscarded,
                format!(
                    "as of block {}, the account is at a state that is neither the base nor the submitted one",
                    observed.block
                ),
            ));
        }
        if observed.block >= evidence.expiration_block {
            return Ok(Observed::Settles(
                ExecutionFailureCode::Expired,
                format!(
                    "as of block {}, at or past the transaction's expiration at {}, the account is still at its base",
                    observed.block, evidence.expiration_block
                ),
            ));
        }
        Ok(Observed::Pending)
    }

    /// Keeps the reservation until the next pass, then gives the lease up: a request arriving
    /// meanwhile reads the execution in flight instead of being turned away as busy, and the
    /// next pass claims the reservation back under a higher fence.
    async fn hold(
        &self,
        state: &AppState,
        record: &ExecutionRecord,
        elector: &dyn LeaderElector,
        lease: Lease,
    ) {
        let account_id = &record.reservation.account_id;
        match state
            .storage
            .renew_execution_reservation(
                account_id,
                &LeaseFence::from(&lease),
                lease.expires_at,
                ExecutionPhase::Reconciling,
            )
            .await
        {
            Ok(ReservationUpdate::Applied) => {}
            Ok(ReservationUpdate::StaleLease | ReservationUpdate::NotActive) => {
                tracing::warn!(%account_id, "the reconciled reservation is no longer this reconciler's");
            }
            Err(error) => {
                tracing::warn!(%account_id, %error, "failed to renew a reconciled reservation");
            }
        }
        release_quietly(elector, lease).await;
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
