use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use crate::jobs::execution_reconcile::{PassKind, Reconciled, Reconciler};
use crate::network::StateVerification;
use crate::services::execution_status::ExecutionState;
use crate::storage::{
    ExecutionFailureCode, LeaseFence, NewExecutionReservation, PromoteWrite, ReservationWrite,
};
use crate::testing::mocks::MockNetworkClient;

use super::SubmissionOutcome;
use super::tests::{ACCOUNT, BASE, Fixture, PROPOSAL, Script, is_submitted_or_terminal};

const LEASE: Duration = Duration::from_millis(300);

struct Faults {
    f: Fixture,
    chain: MockNetworkClient,
}

impl Faults {
    async fn new(script: Script) -> Self {
        let mut f = Fixture::new(script).await;
        let chain = MockNetworkClient::new();
        f.state.network_client = Arc::new(chain.clone());
        f.state.execution.config.lease = LEASE;
        Self { f, chain }
    }

    /// Drives an execution to `submitted` and lets its worker's lease lapse, which is what a
    /// crash right after the send leaves behind.
    async fn submitted_and_abandoned(script: Script) -> Self {
        let faults = Self::new(Script {
            submission: SubmissionOutcome::Unknown {
                reason: "connection reset".to_string(),
            },
            ..script
        })
        .await;
        faults.f.request().await.unwrap();
        let (submitted, _) = faults.f.settle(is_submitted_or_terminal).await;
        assert_eq!(submitted.state, ExecutionState::Submitted);
        faults.wait_for_submit_then_lease_expiry().await;
        faults
    }

    async fn wait_for_submit_then_lease_expiry(&self) {
        for _ in 0..200 {
            if self.f.calls.lock().unwrap().submitted > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(LEASE + Duration::from_millis(100)).await;
    }

    fn chain_shows(&self, verification: Result<StateVerification, String>) {
        self.chain
            .verify_commitment_responses
            .lock()
            .unwrap()
            .push(verification);
    }

    /// The block the chain's next account reads report they were taken at.
    fn chain_at(&self, block: u32) {
        *self.chain.observed_block.lock().unwrap() = block;
    }

    async fn reconcile(&self, kind: PassKind) -> Reconciled {
        let report = Reconciler::new(&self.f.state)
            .pass(&self.f.state, kind)
            .await
            .unwrap();
        let mine: Vec<_> = report
            .into_iter()
            .filter(|(record, _)| record.reservation.account_id == ACCOUNT)
            .map(|(_, outcome)| outcome)
            .collect();
        assert_eq!(mine.len(), 1, "{mine:?}");
        mine[0]
    }

    async fn state(&self) -> ExecutionState {
        self.f.read().await.unwrap().state
    }

    async fn reservation_held(&self) -> bool {
        self.f
            .state
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .is_some()
    }

    fn never_reproduced_again(&self) {
        let calls = self.f.calls.lock().unwrap();
        assert_eq!(
            (calls.prepared, calls.proved, calls.submitted),
            (1, 1, 1),
            "a boundary-crossed attempt is never retried"
        );
    }

    /// A reservation a killed replica left before the boundary, its lease already lapsed.
    async fn crashed_before_the_boundary(&self) {
        let lease = self
            .f
            .state
            .execution
            .leases
            .elector(ACCOUNT, "replica-dead:worker")
            .try_acquire(Duration::from_millis(50))
            .await
            .unwrap()
            .unwrap();
        let now = Utc::now();
        let written = self
            .f
            .state
            .storage
            .create_execution_reservation(NewExecutionReservation {
                account_id: ACCOUNT.to_string(),
                proposal_id: PROPOSAL.to_string(),
                fence: LeaseFence {
                    lease_name: lease.name,
                    holder_id: lease.holder_id,
                    fence_token: lease.fence_token,
                },
                lease_expires_at: lease.expires_at,
                ignored_signatures: 0,
                now,
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(matches!(written, ReservationWrite::Created { .. }));
    }
}

#[tokio::test]
async fn a_live_worker_is_left_alone() {
    let faults = Faults::new(Script {
        submission: SubmissionOutcome::Unknown {
            reason: "timeout".to_string(),
        },
        ..Script::default()
    })
    .await;
    faults.f.request().await.unwrap();
    faults.f.settle(is_submitted_or_terminal).await;
    for _ in 0..200 {
        if faults.f.calls.lock().unwrap().submitted > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    faults
        .f
        .state
        .storage
        .renew_execution_reservation(
            ACCOUNT,
            &faults
                .f
                .state
                .storage
                .load_active_execution(ACCOUNT)
                .await
                .unwrap()
                .unwrap()
                .reservation
                .fence,
            Utc::now() + chrono::Duration::seconds(60),
            crate::storage::ExecutionPhase::Sent,
        )
        .await
        .unwrap();
    assert_eq!(faults.reconcile(PassKind::Steady).await, Reconciled::Owned);
    assert_eq!(faults.state().await, ExecutionState::Submitted);
}

#[tokio::test]
async fn an_unknown_submission_is_settled_only_by_the_chain_and_never_resent() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;

    faults.chain_shows(Ok(StateVerification::Mismatch {
        on_chain: BASE.to_string(),
    }));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Waiting
    );
    assert_eq!(faults.state().await, ExecutionState::Submitted);
    assert!(faults.reservation_held().await);

    faults.chain_at(357);
    faults.chain_shows(Ok(StateVerification::Mismatch {
        on_chain: BASE.to_string(),
    }));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Resolved(ExecutionFailureCode::Expired)
    );
    let settled = faults.f.read().await.unwrap();
    assert_eq!(settled.state, ExecutionState::Failed);
    assert_eq!(
        settled.error.map(|error| error.code),
        Some("GUARDIAN_EXECUTION_EXPIRED".to_string())
    );
    assert!(!settled.proposal_exists);
    assert!(!faults.reservation_held().await);
    faults.never_reproduced_again();
}

#[tokio::test]
async fn switching_execution_off_still_settles_the_executions_in_flight() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    let mut switched_off = faults.f.state.clone();
    switched_off.execution.executor = None;
    faults.chain_at(356);
    faults.chain_shows(Ok(StateVerification::Absent));
    let report = Reconciler::new(&switched_off)
        .pass(&switched_off, PassKind::Startup)
        .await
        .unwrap();
    assert!(report.iter().any(|(record, outcome)| {
        record.reservation.account_id == ACCOUNT
            && *outcome == Reconciled::Resolved(ExecutionFailureCode::Expired)
    }));
    assert!(!faults.reservation_held().await);
}

#[tokio::test]
async fn the_block_before_the_expiration_still_waits() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_at(355);
    faults.chain_shows(Ok(StateVerification::Absent));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Waiting
    );
}

#[tokio::test]
async fn reaching_the_expiration_block_at_the_base_settles_expired() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_at(356);
    faults.chain_shows(Ok(StateVerification::Absent));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Resolved(ExecutionFailureCode::Expired)
    );
}

#[tokio::test]
async fn a_read_from_before_the_execution_block_is_a_lagging_node_not_evidence() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_at(99);
    faults.chain_shows(Ok(StateVerification::Mismatch {
        on_chain: "0x9999".to_string(),
    }));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Waiting
    );
    assert!(faults.reservation_held().await);
}

#[tokio::test]
async fn an_account_moved_elsewhere_discards_the_submission() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_at(100);
    faults.chain_shows(Ok(StateVerification::Mismatch {
        on_chain: "0x9999".to_string(),
    }));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Resolved(ExecutionFailureCode::CandidateDiscarded)
    );
    assert!(!faults.reservation_held().await);
    faults.never_reproduced_again();
}

#[tokio::test]
async fn an_account_at_the_submitted_state_waits_for_promotion_which_alone_commits() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_shows(Ok(StateVerification::Match));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::AwaitingPromotion
    );
    assert_eq!(
        faults.state().await,
        ExecutionState::Submitted,
        "reconciliation never writes committed"
    );
    assert_eq!(faults.f.promote().await, PromoteWrite::Applied);
    assert_eq!(faults.state().await, ExecutionState::Committed);
    assert!(!faults.reservation_held().await);
}

#[tokio::test]
async fn an_unobservable_chain_keeps_the_execution_submitted_until_it_recovers() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_shows(Err("node unavailable".to_string()));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::ObservationUnavailable
    );

    faults.chain_shows(Err("node unavailable".to_string()));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::ObservationUnavailable,
        "passing the expiration height by the clock settles nothing"
    );
    assert_eq!(faults.state().await, ExecutionState::Submitted);
    assert!(faults.reservation_held().await);

    faults.chain_at(400);
    faults.chain_shows(Ok(StateVerification::Absent));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Resolved(ExecutionFailureCode::Expired)
    );
}

#[tokio::test]
async fn a_reconciler_whose_own_lease_lapsed_between_passes_still_settles() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_shows(Ok(StateVerification::Absent));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Waiting
    );
    tokio::time::sleep(LEASE + Duration::from_millis(100)).await;
    faults.chain_at(400);
    faults.chain_shows(Ok(StateVerification::Absent));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Resolved(ExecutionFailureCode::Expired),
        "a pass later than the lease must still be able to write its verdict"
    );
    assert!(!faults.reservation_held().await);
}

/// What a promotion that does not settle the execution leaves: a replica that predates
/// execution, or a filesystem promotion interrupted after its delta write.
async fn promote_without_settling(faults: &Faults) {
    let storage = &faults.f.state.storage;
    let mut delta = storage.pull_delta(ACCOUNT, 1).await.unwrap();
    delta.status = crate::delta_object::DeltaStatus::canonical(Utc::now().to_rfc3339());
    storage.submit_delta(&delta).await.unwrap();
    let mut account = storage.pull_state(ACCOUNT).await.unwrap();
    account.commitment = super::tests::NEW_COMMITMENT.to_string();
    storage.submit_state(&account).await.unwrap();
}

#[tokio::test]
async fn a_promotion_that_left_the_execution_held_is_settled_committed() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    promote_without_settling(&faults).await;
    faults.chain_shows(Ok(StateVerification::Match));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Committed
    );
    assert_eq!(faults.state().await, ExecutionState::Committed);
    assert!(!faults.reservation_held().await);
}

#[tokio::test]
async fn an_account_at_the_submitted_state_before_promotion_is_left_to_promotion() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_shows(Ok(StateVerification::Match));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::AwaitingPromotion,
        "a candidate not yet canonical is promotion's to settle"
    );
    assert!(faults.reservation_held().await);
}

#[tokio::test]
async fn a_reconciler_that_already_holds_the_attempt_keeps_it_across_passes() {
    let faults = Faults::submitted_and_abandoned(Script::default()).await;
    faults.chain_shows(Ok(StateVerification::Absent));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Waiting
    );
    faults.chain_shows(Ok(StateVerification::Absent));
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Waiting
    );
    let holder = faults
        .f
        .state
        .storage
        .load_active_execution(ACCOUNT)
        .await
        .unwrap()
        .unwrap()
        .reservation
        .fence
        .holder_id;
    assert!(holder.ends_with(":reconcile"), "{holder}");
}

#[tokio::test]
async fn a_restart_faster_than_the_lease_still_reports_the_attempt_abandoned() {
    let faults = Faults::new(Script::default()).await;
    let dead = faults
        .f
        .state
        .execution
        .leases
        .elector(ACCOUNT, "replica-dead:worker")
        .try_acquire(Duration::from_secs(3600))
        .await
        .unwrap()
        .unwrap();
    let written = faults
        .f
        .state
        .storage
        .create_execution_reservation(NewExecutionReservation {
            account_id: ACCOUNT.to_string(),
            proposal_id: PROPOSAL.to_string(),
            fence: LeaseFence {
                lease_name: dead.name,
                holder_id: dead.holder_id,
                fence_token: dead.fence_token,
            },
            lease_expires_at: dead.expires_at,
            ignored_signatures: 0,
            now: Utc::now(),
        })
        .await
        .unwrap();
    assert!(matches!(written, ReservationWrite::Created { .. }));

    let mut restarted = faults.f.state.clone();
    restarted.execution.leases = Arc::new(crate::coordination::InMemoryExecutionLeases::new());
    let report = Reconciler::new(&restarted)
        .pass(&restarted, PassKind::Startup)
        .await
        .unwrap();
    assert!(report.iter().any(|(record, outcome)| {
        record.reservation.account_id == ACCOUNT
            && *outcome == Reconciled::Released(ExecutionFailureCode::Abandoned)
    }));
}

#[tokio::test]
async fn an_attempt_interrupted_by_a_restart_is_abandoned_and_the_proposal_stays_executable() {
    let faults = Faults::new(Script::default()).await;
    faults.crashed_before_the_boundary().await;
    assert_eq!(
        faults.reconcile(PassKind::Startup).await,
        Reconciled::Released(ExecutionFailureCode::Abandoned)
    );
    let failed = faults.f.read().await.unwrap();
    assert_eq!(
        failed.error.map(|error| error.code),
        Some("GUARDIAN_EXECUTION_ABANDONED".to_string())
    );
    assert!(failed.proposal_exists);
    assert!(faults.f.request().await.unwrap().newly_accepted);
}

#[tokio::test]
async fn a_pre_boundary_lease_that_lapses_in_steady_state_is_lease_expired() {
    let faults = Faults::new(Script::default()).await;
    faults.crashed_before_the_boundary().await;
    assert_eq!(
        faults.reconcile(PassKind::Steady).await,
        Reconciled::Released(ExecutionFailureCode::LeaseExpired)
    );
    assert!(!faults.reservation_held().await);
    assert_eq!(faults.f.calls.lock().unwrap().submitted, 0);
}

#[tokio::test]
async fn a_proof_that_outlasts_its_lease_keeps_the_reservation() {
    let faults = Faults::new(Script {
        proving_time: LEASE * 3,
        ..Script::default()
    })
    .await;
    faults.f.request().await.unwrap();
    tokio::time::sleep(LEASE * 2).await;
    assert_eq!(faults.reconcile(PassKind::Steady).await, Reconciled::Owned);
    let (settled, _) = faults.f.settle(is_submitted_or_terminal).await;
    assert_eq!(settled.state, ExecutionState::Submitted);
}

/// Leases whose renewals fail a set number of times before reaching the real store, as a
/// database pool timeout would.
struct FlakyRenewals {
    inner: crate::coordination::InMemoryExecutionLeases,
    failures: Arc<std::sync::atomic::AtomicUsize>,
}

struct FlakyElector {
    inner: Arc<dyn crate::coordination::LeaderElector>,
    failures: Arc<std::sync::atomic::AtomicUsize>,
}

impl crate::coordination::ExecutionLeases for FlakyRenewals {
    fn elector(
        &self,
        account_id: &str,
        holder_id: &str,
    ) -> Arc<dyn crate::coordination::LeaderElector> {
        Arc::new(FlakyElector {
            inner: self.inner.elector(account_id, holder_id),
            failures: self.failures.clone(),
        })
    }

    fn is_shared(&self) -> bool {
        false
    }
}

#[async_trait::async_trait]
impl crate::coordination::LeaderElector for FlakyElector {
    async fn try_acquire(
        &self,
        ttl: Duration,
    ) -> crate::error::Result<Option<crate::coordination::Lease>> {
        self.inner.try_acquire(ttl).await
    }

    async fn renew(
        &self,
        lease: &crate::coordination::Lease,
        ttl: Duration,
    ) -> crate::error::Result<bool> {
        use std::sync::atomic::Ordering;
        if self
            .failures
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok()
        {
            return Err(crate::error::GuardianError::StorageError(
                "pool timed out".to_string(),
            ));
        }
        self.inner.renew(lease, ttl).await
    }

    async fn verify_held(&self, lease: &crate::coordination::Lease) -> crate::error::Result<bool> {
        self.inner.verify_held(lease).await
    }

    async fn release(&self, lease: crate::coordination::Lease) -> crate::error::Result<()> {
        self.inner.release(lease).await
    }

    fn supports_fencing(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn a_transient_renewal_failure_does_not_lose_a_long_proof() {
    let mut faults = Faults::new(Script {
        proving_time: LEASE * 3,
        ..Script::default()
    })
    .await;
    faults.f.state.execution.leases = Arc::new(FlakyRenewals {
        inner: crate::coordination::InMemoryExecutionLeases::new(),
        failures: Arc::new(std::sync::atomic::AtomicUsize::new(1)),
    });
    faults.f.request().await.unwrap();
    tokio::time::sleep(LEASE * 2).await;
    assert_eq!(faults.reconcile(PassKind::Steady).await, Reconciled::Owned);
    let (settled, _) = faults.f.settle(is_submitted_or_terminal).await;
    assert_eq!(settled.state, ExecutionState::Submitted);
}

#[tokio::test]
async fn an_expiration_beyond_the_horizon_is_refused_before_proving() {
    for expiration_block in [100 + 28_800, u32::MAX] {
        let faults = Faults::new(Script {
            expiration_block,
            ..Script::default()
        })
        .await;
        faults.f.request().await.unwrap();
        let (failed, _) = faults
            .f
            .settle(|state| state == ExecutionState::Failed)
            .await;
        assert_eq!(
            failed.error.map(|error| error.code),
            Some("GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON".to_string())
        );
        assert_eq!(
            faults.f.calls.lock().unwrap().proved,
            0,
            "refused before any proving"
        );
        assert_eq!(faults.f.calls.lock().unwrap().submitted, 0);
        assert!(faults.f.state.storage.pull_delta(ACCOUNT, 1).await.is_err());
    }
}

#[tokio::test]
async fn a_guardian_switch_during_proving_fails_the_admissibility_recheck() {
    let faults = Faults::new(Script::default()).await;
    faults
        .chain
        .extract_guardian_commitment_responses
        .lock()
        .unwrap()
        .push(Ok(Some("0xanother-guardian".to_string())));
    faults.f.request().await.unwrap();
    let (failed, _) = faults
        .f
        .settle(|state| state == ExecutionState::Failed)
        .await;
    assert_eq!(
        failed.error.map(|error| error.code),
        Some("GUARDIAN_EXECUTION_ACCOUNT_INADMISSIBLE".to_string())
    );
    assert_eq!(faults.f.calls.lock().unwrap().submitted, 0);
}

#[tokio::test]
async fn a_worker_whose_reservation_moved_on_writes_and_sends_nothing() {
    let faults = Faults::new(Script {
        sealing_time: Duration::from_millis(200),
        ..Script::default()
    })
    .await;
    faults.f.request().await.unwrap();
    for _ in 0..200 {
        if faults.f.calls.lock().unwrap().proved > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let worker = faults
        .f
        .state
        .storage
        .load_active_execution(ACCOUNT)
        .await
        .unwrap()
        .unwrap()
        .reservation
        .fence;
    let usurper = LeaseFence {
        holder_id: "replica-c:reconcile".to_string(),
        fence_token: worker.fence_token + 1,
        ..worker.clone()
    };
    faults
        .f
        .state
        .storage
        .claim_execution_reservation(
            ACCOUNT,
            &worker,
            &usurper,
            Utc::now() + chrono::Duration::seconds(60),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        faults.f.calls.lock().unwrap().submitted,
        0,
        "a stale worker never sends"
    );
    assert!(
        faults.f.state.storage.pull_delta(ACCOUNT, 1).await.is_err(),
        "a stale worker never crosses the boundary"
    );
    let record = faults
        .f
        .state
        .storage
        .load_active_execution(ACCOUNT)
        .await
        .unwrap()
        .expect("the new owner still holds the attempt");
    assert_eq!(record.reservation.fence, usurper);
    assert!(
        record.outcome.is_none(),
        "the stale worker wrote no outcome"
    );
}

/// Leases that work until the boundary commit and are gone at the renewal that gates the send:
/// the state a worker is in when its lease lapses or is taken between the commit and the send.
struct LostAfterBoundary {
    leases: crate::coordination::InMemoryExecutionLeases,
    storage: Arc<dyn crate::storage::StorageBackend>,
}

struct LostElector {
    inner: Arc<dyn crate::coordination::LeaderElector>,
    storage: Arc<dyn crate::storage::StorageBackend>,
}

async fn boundary_crossed(storage: &dyn crate::storage::StorageBackend) -> bool {
    storage
        .load_active_execution(ACCOUNT)
        .await
        .unwrap()
        .is_some_and(|record| record.evidence.is_some())
}

#[async_trait::async_trait]
impl crate::coordination::LeaderElector for LostElector {
    async fn try_acquire(
        &self,
        ttl: Duration,
    ) -> crate::error::Result<Option<crate::coordination::Lease>> {
        self.inner.try_acquire(ttl).await
    }
    async fn renew(
        &self,
        lease: &crate::coordination::Lease,
        ttl: Duration,
    ) -> crate::error::Result<bool> {
        if boundary_crossed(self.storage.as_ref()).await {
            return Ok(false);
        }
        self.inner.renew(lease, ttl).await
    }
    async fn verify_held(&self, lease: &crate::coordination::Lease) -> crate::error::Result<bool> {
        self.inner.verify_held(lease).await
    }
    async fn release(&self, lease: crate::coordination::Lease) -> crate::error::Result<()> {
        self.inner.release(lease).await
    }
    fn supports_fencing(&self) -> bool {
        self.inner.supports_fencing()
    }
}

impl crate::coordination::ExecutionLeases for LostAfterBoundary {
    fn elector(
        &self,
        account_id: &str,
        holder_id: &str,
    ) -> Arc<dyn crate::coordination::LeaderElector> {
        Arc::new(LostElector {
            inner: self.leases.elector(account_id, holder_id),
            storage: self.storage.clone(),
        })
    }

    fn is_shared(&self) -> bool {
        self.leases.is_shared()
    }
}

#[tokio::test]
async fn a_worker_that_loses_its_lease_after_the_boundary_never_sends() {
    let mut faults = Faults::new(Script::default()).await;
    faults.f.state.execution.leases = Arc::new(LostAfterBoundary {
        leases: crate::coordination::InMemoryExecutionLeases::new(),
        storage: faults.f.state.storage.clone(),
    });
    faults.f.request().await.unwrap();

    let (reported, _) = faults.f.settle(is_submitted_or_terminal).await;
    assert_eq!(
        reported.state,
        ExecutionState::Submitted,
        "the boundary commit happened"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        faults.f.calls.lock().unwrap().submitted,
        0,
        "a worker that no longer owns the attempt never sends"
    );
    let record = faults
        .f
        .state
        .storage
        .load_active_execution(ACCOUNT)
        .await
        .unwrap()
        .expect("the attempt is left to reconciliation");
    assert!(
        record.evidence.is_some(),
        "the submission evidence is durable"
    );
    assert!(
        record.outcome.is_none(),
        "the stale worker wrote no outcome"
    );
    assert!(
        faults
            .f
            .state
            .storage
            .pull_delta(ACCOUNT, 1)
            .await
            .unwrap()
            .status
            .is_candidate(),
        "the candidate is durable"
    );
}

/// Leases whose renewal at the send passes but hands the reservation to another holder first: a
/// takeover that lands between the worker's lease renewal and its fenced send claim.
struct TakenOverAtTheSend {
    leases: crate::coordination::InMemoryExecutionLeases,
    storage: Arc<dyn crate::storage::StorageBackend>,
}

struct TakingElector {
    inner: Arc<dyn crate::coordination::LeaderElector>,
    storage: Arc<dyn crate::storage::StorageBackend>,
}

#[async_trait::async_trait]
impl crate::coordination::LeaderElector for TakingElector {
    async fn try_acquire(
        &self,
        ttl: Duration,
    ) -> crate::error::Result<Option<crate::coordination::Lease>> {
        self.inner.try_acquire(ttl).await
    }
    async fn renew(
        &self,
        lease: &crate::coordination::Lease,
        ttl: Duration,
    ) -> crate::error::Result<bool> {
        if !boundary_crossed(self.storage.as_ref()).await {
            return self.inner.renew(lease, ttl).await;
        }
        let current = self
            .storage
            .load_active_execution(ACCOUNT)
            .await
            .unwrap()
            .expect("the attempt holds a reservation")
            .reservation
            .fence;
        let successor = LeaseFence {
            holder_id: "successor".to_string(),
            fence_token: current.fence_token + 1,
            ..current.clone()
        };
        let claimed = self
            .storage
            .claim_execution_reservation(
                ACCOUNT,
                &current,
                &successor,
                Utc::now() + chrono::Duration::seconds(60),
            )
            .await
            .unwrap();
        assert_eq!(claimed, crate::storage::ClaimWrite::Claimed);
        self.inner.renew(lease, ttl).await
    }
    async fn verify_held(&self, lease: &crate::coordination::Lease) -> crate::error::Result<bool> {
        self.inner.verify_held(lease).await
    }
    async fn release(&self, lease: crate::coordination::Lease) -> crate::error::Result<()> {
        self.inner.release(lease).await
    }
    fn supports_fencing(&self) -> bool {
        self.inner.supports_fencing()
    }
}

impl crate::coordination::ExecutionLeases for TakenOverAtTheSend {
    fn elector(
        &self,
        account_id: &str,
        holder_id: &str,
    ) -> Arc<dyn crate::coordination::LeaderElector> {
        Arc::new(TakingElector {
            inner: self.leases.elector(account_id, holder_id),
            storage: self.storage.clone(),
        })
    }

    fn is_shared(&self) -> bool {
        self.leases.is_shared()
    }
}

#[tokio::test]
async fn a_worker_whose_reservation_is_taken_just_before_the_send_never_sends() {
    let mut faults = Faults::new(Script::default()).await;
    faults.f.state.execution.leases = Arc::new(TakenOverAtTheSend {
        leases: crate::coordination::InMemoryExecutionLeases::new(),
        storage: faults.f.state.storage.clone(),
    });
    faults.f.request().await.unwrap();

    faults.f.settle(is_submitted_or_terminal).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        faults.f.calls.lock().unwrap().submitted,
        0,
        "the fenced renewal refuses the stale worker, so it never sends"
    );
    let record = faults
        .f
        .state
        .storage
        .load_active_execution(ACCOUNT)
        .await
        .unwrap()
        .expect("the attempt is left to its new owner");
    assert_eq!(record.reservation.fence.holder_id, "successor");
    assert!(record.outcome.is_none(), "the stale worker wrote nothing");
}
