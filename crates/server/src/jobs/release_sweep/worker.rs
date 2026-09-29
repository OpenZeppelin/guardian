//! The release sweep task: one lease holder and one paced loop. Each turn
//! visits a single account: a due confirmation re-check or the next
//! account of the rotation (one walk of the fleet per `rotation_seconds`).
//! When both are due they take turns, and two visits are never closer
//! than `1 / max_rate_per_second` whatever their kind, so neither can
//! starve the other and together they never raise the rate. The cursor
//! advances per visited account and survives a lost lease within the
//! process, so the walk resumes where it stopped; a new process starts a
//! new walk.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, sleep, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::coordination::LeaderElector;
use crate::error::Result;
use crate::jobs::canonicalization::spawn_lease_renewal;
use crate::metrics::labels::RunOutcome;
use crate::release_sweep::ReleaseSweepConfig;
use crate::state::AppState;

use super::sweep::{ReleaseSweeper, SweepState, VisitOutcome};

/// Lease TTL / renew cadence: the holder renews every third of the TTL,
/// like the canonicalization worker, so one missed renewal never loses
/// the lease.
const LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(10);

/// Wait before retrying to acquire the lease: a lost holder's lease
/// expires within one TTL.
const LEASE_RETRY_INTERVAL: Duration = LEASE_TTL;

pub fn start_release_sweep_worker(
    state: AppState,
    config: ReleaseSweepConfig,
    leader: Arc<dyn LeaderElector>,
) {
    tokio::spawn(async move {
        run_worker(state, config, leader).await;
    });
}

async fn run_worker(state: AppState, config: ReleaseSweepConfig, leader: Arc<dyn LeaderElector>) {
    let sweeper = ReleaseSweeper::new(
        state,
        config.confirmations,
        config.recheck(),
        Arc::new(SweepState::default()),
    );
    let mut rotation = Rotation::new(&config);

    loop {
        let lease = match leader.try_acquire(LEASE_TTL).await {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                sleep(LEASE_RETRY_INTERVAL).await;
                continue;
            }
            Err(error) => {
                tracing::warn!(error = %error, "Failed to acquire the release sweep lease");
                sleep(LEASE_RETRY_INTERVAL).await;
                continue;
            }
        };
        tracing::info!(holder = %lease.holder_id, "Release sweep lease acquired");

        let cancel = CancellationToken::new();
        let renewal = spawn_lease_renewal(
            leader.clone(),
            lease.clone(),
            LEASE_TTL,
            LEASE_RENEW_INTERVAL,
            cancel.clone(),
            "Release sweep",
        );
        {
            // If the loop unwinds (a panic), the guard stops the renewal,
            // so the lease expires and another replica takes over instead
            // of this one holding it with no sweep running.
            let _stop_renewal = cancel.clone().drop_guard();
            hold_lease(&sweeper, &config, &mut rotation, &cancel).await;
        }

        let _ = renewal.await;
        if let Err(error) = leader.release(lease).await {
            tracing::warn!(error = %error, "Failed to release the release sweep lease");
        }
        tracing::info!("Release sweep lease released; retrying acquisition");
        sleep(LEASE_RETRY_INTERVAL).await;
    }
}

/// When the next visit may start: the earlier of the rotation's next turn
/// and the earliest due re-check, but never closer than `min_spacing` to
/// the last visit.
fn next_wake(
    rotation_turn: Instant,
    recheck_due: Option<Instant>,
    last_visit: Option<Instant>,
    min_spacing: Duration,
) -> Instant {
    let due = recheck_due.map_or(rotation_turn, |recheck| recheck.min(rotation_turn));
    last_visit.map_or(due, |last| due.max(last + min_spacing))
}

/// Drive the paced loop while the lease is held. Returns when the lease
/// is lost (renewal cancelled the token).
async fn hold_lease(
    sweeper: &ReleaseSweeper,
    config: &ReleaseSweepConfig,
    rotation: &mut Rotation,
    cancel: &CancellationToken,
) {
    let mut last_visit: Option<Instant> = None;
    let mut last_was_recheck = false;

    loop {
        let wake = next_wake(
            rotation.next_turn,
            sweeper.sweep_state().next_recheck_due(),
            last_visit,
            config.min_spacing(),
        );
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = sleep_until(wake) => {}
        }

        // One visit per turn. When a re-check and the rotation are both
        // due they alternate, so a burst of re-checks (many accounts in
        // confirmation, a slow node) delays the rotation by at most one
        // visit per turn instead of stalling it.
        let now = Instant::now();
        let rotation_due = now >= rotation.next_turn;
        let take_recheck = !(rotation_due && last_was_recheck);
        if take_recheck && let Some(account_id) = sweeper.sweep_state().pop_due_recheck(now) {
            sweeper.visit_absorbing(&account_id).await;
            last_visit = Some(now);
            last_was_recheck = true;
            continue;
        }
        last_was_recheck = false;
        if !rotation_due {
            continue;
        }
        match rotation.step(sweeper, config, now).await {
            Ok(Step::Visited) => last_visit = Some(now),
            Ok(Step::Idle) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Release sweep rotation step failed; backing off"
                );
                rotation.next_turn = now + config.recheck();
            }
        }
    }
}

/// Outcome of one rotation step.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// One account was visited; the next turn is one pacing gap later.
    Visited,
    /// Nothing to do until `next_turn` (the current rotation is complete,
    /// or the fleet is empty).
    Idle,
}

/// The paced walk of the fleet. Pacing spreads one walk over
/// `rotation_seconds` (spacing = rotation / fleet size), never tighter
/// than `min_spacing()`, so a fleet too large for the window at the visit
/// rate takes longer than the window. The fleet is recounted on every
/// page refill, so a fleet that grows during a walk is paced for its new
/// size instead of the old one.
struct Rotation {
    rotation: Duration,
    min_spacing: Duration,
    page_size: u32,
    spacing: Duration,
    /// Account ids of the current page. Only ids: each visit reads its
    /// row fresh.
    buffer: VecDeque<String>,
    /// The last account id visited in the current walk.
    cursor: Option<String>,
    started_at: Option<Instant>,
    /// When the next walk may start.
    next_start: Instant,
    /// When the rotation's next turn is due.
    next_turn: Instant,
    visited: usize,
    /// Visits that could not check their account (failed or deferred).
    unchecked: usize,
}

impl Rotation {
    fn new(config: &ReleaseSweepConfig) -> Self {
        let now = Instant::now();
        Self {
            rotation: config.rotation(),
            min_spacing: config.min_spacing(),
            page_size: config.page_size.max(1),
            spacing: config.min_spacing(),
            buffer: VecDeque::new(),
            cursor: None,
            started_at: None,
            next_start: now,
            next_turn: now,
            visited: 0,
            unchecked: 0,
        }
    }

    /// Pacing gap for a fleet of `fleet_size` accounts.
    fn spacing_for(&self, fleet_size: usize) -> Duration {
        let spread = self.rotation.as_secs_f64() / fleet_size.max(1) as f64;
        Duration::from_secs_f64(spread).max(self.min_spacing)
    }

    async fn step(
        &mut self,
        sweeper: &ReleaseSweeper,
        config: &ReleaseSweepConfig,
        now: Instant,
    ) -> Result<Step> {
        if self.started_at.is_none() {
            if now < self.next_start {
                self.next_turn = self.next_start;
                return Ok(Step::Idle);
            }
            self.started_at = Some(now);
            self.visited = 0;
            self.unchecked = 0;
        }

        if self.buffer.is_empty() {
            let fleet_size = sweeper.fleet_size().await?;
            self.spacing = self.spacing_for(fleet_size);
            let page = sweeper
                .list_page(self.cursor.as_deref(), self.page_size)
                .await?;
            if page.is_empty() {
                self.complete(config);
                return Ok(Step::Idle);
            }
            if self.visited == 0 {
                tracing::info!(
                    fleet_size,
                    spacing_ms = self.spacing.as_millis(),
                    rotation_seconds = config.rotation_seconds,
                    "Release sweep rotation started"
                );
            }
            self.buffer.extend(page);
        }

        let account_id = self
            .buffer
            .pop_front()
            .expect("buffer refilled above or the rotation completed");
        let outcome = sweeper.visit_absorbing(&account_id).await;
        // Advance the cursor only past the account actually visited, so
        // a lost lease or a failed page mid-walk never skips accounts.
        self.cursor = Some(account_id);
        self.visited += 1;
        self.unchecked += usize::from(outcome != VisitOutcome::Checked);
        self.next_turn = now + self.spacing;
        Ok(Step::Visited)
    }

    fn complete(&mut self, config: &ReleaseSweepConfig) {
        let started_at = self.started_at.take().unwrap_or_else(Instant::now);
        let elapsed = started_at.elapsed();
        let outcome = if self.unchecked > 0 {
            RunOutcome::Partial
        } else {
            RunOutcome::Completed
        };
        metrics::counter!(
            crate::metrics::names::RELEASE_SWEEP_ROTATIONS_TOTAL,
            crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
        )
        .increment(1);
        metrics::histogram!(crate::metrics::names::RELEASE_SWEEP_ROTATION_DURATION_SECONDS)
            .record(elapsed.as_secs_f64());
        tracing::info!(
            accounts = self.visited,
            unchecked_accounts = self.unchecked,
            duration_seconds = elapsed.as_secs_f64(),
            "Release sweep rotation completed"
        );
        self.cursor = None;
        self.buffer.clear();
        self.next_start = started_at + config.rotation();
        self.next_turn = self.next_start;
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::metadata::auth::{Auth, Credentials};
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::network::{NetworkClient, OnChainGuardianBinding, RpcReadMode, StateVerification};
    use crate::state_object::StateObject;
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Mutex as StdMutex;

    fn miden_meta(account_id: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2026-09-01T00:00:00Z".into(),
            updated_at: "2026-09-01T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }
    }

    fn sweeper(metadata: MockMetadataStore) -> (ReleaseSweeper, Arc<MockMetadataStore>) {
        let metadata = Arc::new(metadata);
        let state = create_test_app_state_with_mocks(
            Arc::new(MockStorageBackend::new()),
            Arc::new(MockNetworkClient::new()),
            metadata.clone(),
        );
        (
            ReleaseSweeper::new(
                state,
                1,
                Duration::from_secs(60),
                Arc::new(SweepState::default()),
            ),
            metadata,
        )
    }

    #[test]
    fn spacing_spreads_the_rotation_over_the_fleet_but_never_below_the_rate_floor() {
        let config = ReleaseSweepConfig::default()
            .with_rotation_seconds(3600)
            .with_max_rate_per_second(4);
        let rotation = Rotation::new(&config);
        // 360 accounts over an hour: one every 10 s.
        assert_eq!(rotation.spacing_for(360), Duration::from_secs(10));
        // 100k accounts would need 28/s; the rate floor caps it at 4/s.
        assert_eq!(rotation.spacing_for(100_000), Duration::from_millis(250));
        // An empty fleet does not divide by zero.
        assert_eq!(rotation.spacing_for(0), Duration::from_secs(3600));
    }

    #[tokio::test(start_paused = true)]
    async fn next_wake_prefers_a_due_recheck_but_keeps_the_rate_floor() {
        let now = Instant::now();
        let floor = Duration::from_millis(200);
        let rotation_turn = now + Duration::from_secs(100);
        // No re-check: the rotation's turn.
        assert_eq!(next_wake(rotation_turn, None, None, floor), rotation_turn);
        // An earlier re-check wins...
        let recheck = now + Duration::from_secs(5);
        assert_eq!(
            next_wake(rotation_turn, Some(recheck), None, floor),
            recheck
        );
        // ...but never closer than the rate floor to the last visit.
        assert_eq!(
            next_wake(rotation_turn, Some(now), Some(now), floor),
            now + floor
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rotation_advances_the_cursor_per_visited_account_and_idles_until_the_next_start() {
        // Three accounts, page size 2, rotation 300 s: each step visits
        // one account and parks the cursor on it; the fourth step finds
        // the walk complete and idles until start + rotation.
        let config = ReleaseSweepConfig::default()
            .with_rotation_seconds(300)
            .with_page_size(2);
        let (sweeper, metadata) = sweeper(
            MockMetadataStore::new()
                .with_count_release_sweep_accounts(Ok(3))
                .with_count_release_sweep_accounts(Ok(3))
                .with_count_release_sweep_accounts(Ok(3))
                // page 3 (popped last): empty
                .with_list_release_sweep_ids(Ok(vec![]))
                // page 2
                .with_list_release_sweep_ids(Ok(vec!["0xc".into()]))
                // page 1
                .with_list_release_sweep_ids(Ok(vec!["0xa".into(), "0xb".into()])),
        );
        let mut rotation = Rotation::new(&config);
        let start = Instant::now();

        assert_eq!(
            rotation.step(&sweeper, &config, start).await.unwrap(),
            Step::Visited
        );
        assert_eq!(rotation.cursor.as_deref(), Some("0xa"));
        assert_eq!(
            rotation.spacing,
            Duration::from_secs(100),
            "300 s over 3 accounts"
        );
        assert_eq!(rotation.next_turn, start + Duration::from_secs(100));
        assert_eq!(
            rotation.step(&sweeper, &config, start).await.unwrap(),
            Step::Visited
        );
        assert_eq!(rotation.cursor.as_deref(), Some("0xb"));
        assert_eq!(
            rotation.step(&sweeper, &config, start).await.unwrap(),
            Step::Visited
        );
        assert_eq!(rotation.cursor.as_deref(), Some("0xc"));

        assert_eq!(
            rotation.step(&sweeper, &config, start).await.unwrap(),
            Step::Idle
        );
        assert_eq!(rotation.next_turn, start + Duration::from_secs(300));
        assert_eq!(rotation.cursor, None, "a completed walk resets the cursor");
        assert_eq!(
            metadata.get_list_release_sweep_ids_calls(),
            vec![(None, 2), (Some("0xb".into()), 2), (Some("0xc".into()), 2)]
        );
        assert_eq!(
            metadata.get_calls.lock().unwrap().clone(),
            vec!["0xa", "0xb", "0xc"],
            "each visit reads its row by id"
        );

        // Before the next start the rotation stays idle without listing.
        assert_eq!(
            rotation.step(&sweeper, &config, start).await.unwrap(),
            Step::Idle
        );
        assert_eq!(metadata.get_list_release_sweep_ids_calls().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_page_keeps_the_cursor_so_the_walk_resumes() {
        let config = ReleaseSweepConfig::default().with_page_size(1);
        let (sweeper, _metadata) = sweeper(
            MockMetadataStore::new()
                .with_count_release_sweep_accounts(Ok(2))
                .with_count_release_sweep_accounts(Ok(2))
                .with_list_release_sweep_ids(Err("store down".into()))
                .with_list_release_sweep_ids(Ok(vec!["0xa".into()])),
        );
        let mut rotation = Rotation::new(&config);
        let now = Instant::now();
        assert_eq!(
            rotation.step(&sweeper, &config, now).await.unwrap(),
            Step::Visited
        );
        assert!(rotation.step(&sweeper, &config, now).await.is_err());
        assert_eq!(
            rotation.cursor.as_deref(),
            Some("0xa"),
            "the cursor stays on the last visited account"
        );
        assert!(
            rotation.started_at.is_some(),
            "the rotation is still in progress"
        );
    }

    /// Network client whose every probe takes `probe_delay` (a slow node)
    /// and records which account was probed when. Accounts named `hot*`
    /// show a foreign guardian key in published storage, always at the
    /// same block, so their confirmation streak stays open and they are
    /// re-checked for the whole run.
    struct SlowNetwork {
        probe_delay: Duration,
        probes: StdMutex<Vec<(String, Instant)>>,
    }

    #[async_trait::async_trait]
    impl NetworkClient for SlowNetwork {
        fn get_state_commitment(
            &self,
            _: &str,
            _: &serde_json::Value,
        ) -> std::result::Result<String, String> {
            unimplemented!()
        }
        async fn verify_commitment(
            &self,
            account_id: &str,
            _: &str,
            _: RpcReadMode,
        ) -> std::result::Result<StateVerification, String> {
            self.probes
                .lock()
                .unwrap()
                .push((account_id.to_string(), Instant::now()));
            tokio::time::sleep(self.probe_delay).await;
            Ok(if account_id.starts_with("hot") {
                StateVerification::Mismatch {
                    on_chain: "0xmoved".into(),
                }
            } else {
                StateVerification::Match
            })
        }
        async fn fetch_on_chain_guardian_binding(
            &self,
            _: &str,
            _: RpcReadMode,
        ) -> std::result::Result<OnChainGuardianBinding, String> {
            Ok(OnChainGuardianBinding::Visible {
                on_chain_commitment: "0xmoved".into(),
                guardian_commitment: Some("0xforeign".into()),
                nonce: 1,
                block_num: 7,
            })
        }
        fn verify_delta(
            &self,
            _: &str,
            _: &serde_json::Value,
            _: &serde_json::Value,
        ) -> std::result::Result<(), String> {
            unimplemented!()
        }
        fn apply_delta(
            &self,
            _: &serde_json::Value,
            _: &serde_json::Value,
        ) -> std::result::Result<(serde_json::Value, String), String> {
            unimplemented!()
        }
        fn merge_deltas(
            &self,
            _: Vec<serde_json::Value>,
        ) -> std::result::Result<serde_json::Value, String> {
            unimplemented!()
        }
        fn delta_proposal_id(
            &self,
            _: &str,
            _: u64,
            _: &serde_json::Value,
        ) -> std::result::Result<String, String> {
            unimplemented!()
        }
        fn validate_account_id(&self, _: &str) -> std::result::Result<(), String> {
            unimplemented!()
        }
        fn validate_credential(
            &self,
            _: &serde_json::Value,
            _: &Credentials,
            _: &Auth,
        ) -> std::result::Result<(), String> {
            unimplemented!()
        }
        fn validate_guardian_commitment(
            &self,
            _: &serde_json::Value,
            _: &str,
        ) -> std::result::Result<(), String> {
            unimplemented!()
        }
        async fn should_update_auth(
            &self,
            _: &serde_json::Value,
            _: &Auth,
        ) -> std::result::Result<Option<Auth>, String> {
            unimplemented!()
        }
    }

    fn state_row() -> StateObject {
        StateObject {
            account_id: "any".into(),
            commitment: "0xstored".into(),
            state_json: serde_json::json!({}),
            created_at: "2026-09-01T00:00:00Z".into(),
            updated_at: "2026-09-01T00:00:00Z".into(),
            auth_scheme: String::new(),
        }
    }

    /// Run the real `hold_lease` for `window` of virtual time over a
    /// three-account fleet (`a`, `b`, `c`, rotation 300 s) plus `hot`
    /// accounts stuck in confirmation, against a node whose every probe
    /// takes `probe_delay`. Returns the probes in order.
    async fn run_loop(
        hot: &[&str],
        probe_delay: Duration,
        recheck: Duration,
        window: Duration,
    ) -> Vec<(String, Instant)> {
        let config = ReleaseSweepConfig::default()
            .with_rotation_seconds(300) // 3 accounts -> 100 s spacing
            .with_recheck_seconds(recheck.as_secs())
            .with_max_rate_per_second(5)
            .with_page_size(10);
        let mut storage = MockStorageBackend::new();
        for _ in 0..2_000 {
            storage = storage.with_pull_state(Ok(state_row()));
        }
        let mut metadata = MockMetadataStore::new()
            .with_count_release_sweep_accounts(Ok(3))
            .with_list_release_sweep_ids(Ok(vec![]))
            .with_list_release_sweep_ids(Ok(vec!["a".into(), "b".into(), "c".into()]));
        for id in ["a", "b", "c"].iter().chain(hot) {
            metadata = metadata.with_row(miden_meta(id));
        }
        let network = Arc::new(SlowNetwork {
            probe_delay,
            probes: StdMutex::new(vec![]),
        });
        let state = create_test_app_state_with_mocks(
            Arc::new(storage),
            network.clone(),
            Arc::new(metadata),
        );
        let cancel = CancellationToken::new();
        let sweeper =
            ReleaseSweeper::new(state, 2, config.recheck(), Arc::new(SweepState::default()));
        // Enroll the hot accounts: one observation each, re-check queued.
        for id in hot {
            sweeper.visit_absorbing(id).await;
        }
        network.probes.lock().unwrap().clear();

        let mut rotation = Rotation::new(&config);
        let canceller = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                sleep(window).await;
                cancel.cancel();
            })
        };
        hold_lease(&sweeper, &config, &mut rotation, &cancel).await;
        canceller.await.unwrap();
        let probes = network.probes.lock().unwrap().clone();
        probes
    }

    fn visits_of(probes: &[(String, Instant)], ids: &[&str]) -> Vec<String> {
        probes
            .iter()
            .filter(|(id, _)| ids.contains(&id.as_str()))
            .map(|(id, _)| id.clone())
            .collect()
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_node_with_an_open_streak_never_starves_the_rotation() {
        // Every probe outlasts the re-check delay (70 s vs 60 s): the
        // condition that starved the rotation behind the old hot pass.
        let probes = run_loop(
            &["hot"],
            Duration::from_secs(70),
            Duration::from_secs(60),
            Duration::from_secs(3600),
        )
        .await;
        assert_eq!(
            visits_of(&probes, &["a", "b", "c"]),
            vec!["a", "b", "c"],
            "the rotation walked the whole fleet"
        );
        assert!(
            visits_of(&probes, &["hot"]).len() >= 10,
            "the open streak kept being re-checked"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_rechecks_takes_turns_with_the_rotation() {
        // Eight accounts in confirmation with 20 s probes and a 30 s
        // re-check delay: some re-check is due at every turn. The rotation
        // still gets every other turn while its own turn is due.
        let hot = [
            "hot1", "hot2", "hot3", "hot4", "hot5", "hot6", "hot7", "hot8",
        ];
        let probes = run_loop(
            &hot,
            Duration::from_secs(20),
            Duration::from_secs(30),
            Duration::from_secs(1800),
        )
        .await;
        let order: Vec<String> = probes.iter().map(|(id, _)| id.clone()).collect();
        assert_eq!(visits_of(&probes, &["a", "b", "c"]), vec!["a", "b", "c"]);
        for pair in order.windows(2) {
            assert!(
                !(["a", "b", "c"].contains(&pair[0].as_str())
                    && ["a", "b", "c"].contains(&pair[1].as_str())),
                "a rotation visit is followed by a due re-check: {order:?}"
            );
        }
        let rotation_positions: Vec<usize> = order
            .iter()
            .enumerate()
            .filter(|(_, id)| ["a", "b", "c"].contains(&id.as_str()))
            .map(|(position, _)| position)
            .collect();
        assert!(
            rotation_positions.first().is_some_and(|first| *first <= 1),
            "the rotation's first due turn is served within one re-check: {order:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn visits_never_come_closer_than_the_rate_floor() {
        // Instant probes and a zero-length re-check backlog: the only
        // thing spacing the visits is `max_rate_per_second` (5/s).
        let probes = run_loop(
            &["hot1", "hot2", "hot3"],
            Duration::ZERO,
            Duration::from_secs(1),
            Duration::from_secs(120),
        )
        .await;
        assert!(
            probes.len() > 10,
            "the loop kept visiting: {}",
            probes.len()
        );
        for pair in probes.windows(2) {
            let gap = pair[1].1.duration_since(pair[0].1);
            assert!(
                gap >= Duration::from_millis(200),
                "two visits {gap:?} apart: {pair:?}"
            );
        }
    }
}
