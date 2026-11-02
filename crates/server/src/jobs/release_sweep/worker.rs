//! The release sweep task: one lease holder and one paced loop. Each turn
//! visits a single account: a due confirmation re-check, a due block watch
//! observation (issue #585), or the next account of the rotation (one walk
//! of the fleet per `rotation_seconds`). When several are due they take
//! turns in that order, and two visits are never closer than
//! `1 / max_rate_per_second` whatever their kind, so none can starve the
//! others and together they never raise the rate. The rotation's cursor
//! advances per visited account and survives a lost lease within the
//! process, so the walk resumes where it stopped; a new process starts a
//! new walk.
//!
//! With block watch enabled, the lease holder also runs the block watch
//! consumer, which records the observations this loop visits. Both stop
//! with the lease; a consumer that loses the lease (its cursor write is
//! refused) or fails stops the whole holder, so the next holder starts
//! both again.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, sleep, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::builder::clock::Clock;
use crate::coordination::{BlockWatchStore, LeaderElector, Lease, Observation};
use crate::error::Result;
use crate::jobs::block_watch::{BlockWatchSignals, Consumer, Stopped};
use crate::jobs::canonicalization::spawn_lease_renewal;
use crate::metrics::labels::{BlockWatchVisitOutcome, RunOutcome};
use crate::metrics::names;
use crate::release_sweep::ReleaseSweepConfig;
use crate::state::AppState;
use crate::storage::LeaseFence;

use super::sweep::{ReleaseSweeper, SweepState, VisitOutcome};

/// Lease TTL / renew cadence: the holder renews every third of the TTL,
/// like the canonicalization worker, so one missed renewal never loses
/// the lease.
const LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(10);

/// Wait before retrying to acquire the lease: a lost holder's lease
/// expires within one TTL.
const LEASE_RETRY_INTERVAL: Duration = LEASE_TTL;

/// "Not scheduled": far enough out that the loop only wakes for something
/// else, close enough for the timer.
const NEVER: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Longest wait before the loop retries an observation store it could not
/// read.
const OBSERVATION_STORE_RETRY: Duration = Duration::from_secs(30);

/// Longest retry delay of an observation whose visits keep failing.
const MAX_OBSERVATION_BACKOFF: Duration = Duration::from_secs(60 * 60);

pub fn start_release_sweep_worker(
    state: AppState,
    config: ReleaseSweepConfig,
    leader: Arc<dyn LeaderElector>,
) {
    tokio::spawn(async move {
        run_worker(state, config, leader).await;
    });
}

fn fence_of(lease: &Lease) -> LeaseFence {
    LeaseFence {
        lease_name: lease.name.clone(),
        holder_id: lease.holder_id.clone(),
        fence_token: lease.fence_token,
    }
}

async fn run_worker(state: AppState, config: ReleaseSweepConfig, leader: Arc<dyn LeaderElector>) {
    let sweeper = ReleaseSweeper::new(
        state.clone(),
        config.confirmations,
        config.recheck(),
        Arc::new(SweepState::default()),
    );
    let mut rotation = Rotation::new(&config);
    let signals = Arc::new(BlockWatchSignals::default());
    let observations = state.block_watch.as_ref().map(|handle| ObservationQueue {
        store: handle.store.clone(),
        signals: signals.clone(),
        clock: state.clock.clone(),
        recheck: config.recheck(),
        max_attempts: handle.config.max_attempts,
    });
    let mut first_hold = true;

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
        let consumer = state.block_watch.as_ref().map(|handle| {
            // Observations held in memory did not survive the process that
            // recorded them: its first consumer asks for one rotation.
            let consumer = Consumer::new(
                state.clone(),
                handle.clone(),
                signals.clone(),
                fence_of(&lease),
                first_hold && !handle.durable_observations,
            );
            spawn_consumer(consumer, cancel.clone())
        });
        first_hold = false;
        {
            // If the loop unwinds (a panic), the guard stops the renewal,
            // so the lease expires and another replica takes over instead
            // of this one holding it with no sweep running.
            let _stop_renewal = cancel.clone().drop_guard();
            hold_lease(
                &sweeper,
                &config,
                &mut rotation,
                &cancel,
                observations.as_ref(),
            )
            .await;
        }

        let _ = renewal.await;
        if let Some(consumer) = consumer {
            let _ = consumer.await;
        }
        if let Err(error) = leader.release(lease).await {
            tracing::warn!(error = %error, "Failed to release the release sweep lease");
        }
        tracing::info!("Release sweep lease released; retrying acquisition");
        sleep(LEASE_RETRY_INTERVAL).await;
    }
}

/// Run the block watch consumer until the lease is cancelled. A consumer
/// that stops on its own (its cursor write was refused, or it panicked)
/// cancels the lease, so the holder steps down instead of sweeping without
/// a feed.
fn spawn_consumer(consumer: Consumer, cancel: CancellationToken) -> tokio::task::JoinHandle<()> {
    let inner = tokio::spawn(consumer.run(cancel.clone()));
    tokio::spawn(async move {
        match inner.await {
            Ok(Stopped::Cancelled) => {}
            Ok(Stopped::LostLease) => cancel.cancel(),
            Err(error) => {
                tracing::error!(
                    event = "block_watch_consumer_failed",
                    error = %error,
                    "Block watch consumer stopped unexpectedly; giving up the release sweep \
                     lease so it restarts"
                );
                cancel.cancel();
            }
        }
    })
}

/// The block watch observations the loop visits, and what it needs to
/// schedule their retries.
struct ObservationQueue {
    store: Arc<dyn BlockWatchStore>,
    signals: Arc<BlockWatchSignals>,
    clock: Arc<dyn Clock>,
    recheck: Duration,
    max_attempts: u32,
}

impl ObservationQueue {
    /// When the earliest observation is due, on the loop's clock. A store
    /// that cannot be read is retried after a short wait.
    async fn next_due(&self) -> Option<Instant> {
        match self.store.next_observation_due_at().await {
            Ok(Some(due_at)) => {
                let wait = (due_at - self.clock.now())
                    .to_std()
                    .unwrap_or(Duration::ZERO);
                Some(Instant::now() + wait)
            }
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "Failed to read the block watch observations; retrying"
                );
                Some(Instant::now() + OBSERVATION_STORE_RETRY)
            }
        }
    }

    /// Visit the due observation of the lowest block, if any. `false` when
    /// there was none to visit.
    async fn visit_next(&self, sweeper: &ReleaseSweeper) -> bool {
        let observation = match self.store.next_due_observation(self.clock.now()).await {
            Ok(Some(observation)) => observation,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!(error = %error, "Failed to read the next block watch observation");
                return false;
            }
        };
        let outcome = match sweeper
            .visit_observed(
                &observation.account_id,
                &observation.on_chain_commitment,
                observation.block_num,
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::error!(
                    account_id = %observation.account_id,
                    block_num = observation.block_num,
                    error = %error,
                    "Release sweep failed for an observed account"
                );
                VisitOutcome::Failed
            }
        };
        let recorded = self.settle(&observation, outcome).await;
        metrics::counter!(
            names::BLOCK_WATCH_OBSERVATION_VISITS_TOTAL,
            names::LABEL_OUTCOME => recorded.as_str()
        )
        .increment(1);
        tracing::debug!(
            event = "block_watch_observation_visited",
            account_id = %observation.account_id,
            block_num = observation.block_num,
            outcome = recorded.as_str(),
            attempts = observation.attempts,
            "Visited a block watch observation"
        );
        if let Ok(pending) = self.store.count_pending_observations().await {
            metrics::gauge!(names::BLOCK_WATCH_OBSERVATIONS_PENDING).set(pending as f64);
        }
        true
    }

    /// Delete a checked observation; retry one that could not be checked,
    /// backing off, until it runs out of attempts.
    async fn settle(
        &self,
        observation: &Observation,
        outcome: VisitOutcome,
    ) -> BlockWatchVisitOutcome {
        let account_id = observation.account_id.as_str();
        let block_num = observation.block_num;
        let (recorded, result) = match outcome {
            VisitOutcome::Checked => (
                BlockWatchVisitOutcome::Checked,
                self.store.delete_observation(account_id, block_num).await,
            ),
            VisitOutcome::Deferred | VisitOutcome::Failed
                if observation.attempts.saturating_add(1) >= self.max_attempts =>
            {
                tracing::warn!(
                    event = "block_watch_observation_dropped",
                    account_id = %account_id,
                    block_num,
                    attempts = observation.attempts.saturating_add(1),
                    "Dropped a block watch observation that could not be checked; the \
                     release sweep rotation remains the backstop"
                );
                (
                    BlockWatchVisitOutcome::Dropped,
                    self.store.delete_observation(account_id, block_num).await,
                )
            }
            VisitOutcome::Deferred | VisitOutcome::Failed => {
                let wait = observation_backoff(self.recheck, observation.attempts);
                let next_attempt_at = self.clock.now()
                    + chrono::Duration::from_std(wait).unwrap_or(chrono::Duration::hours(1));
                let recorded = if outcome == VisitOutcome::Deferred {
                    BlockWatchVisitOutcome::Deferred
                } else {
                    BlockWatchVisitOutcome::Failed
                };
                (
                    recorded,
                    self.store
                        .defer_observation(account_id, block_num, next_attempt_at)
                        .await,
                )
            }
        };
        if let Err(error) = result {
            tracing::warn!(
                account_id = %account_id,
                block_num,
                error = %error,
                "Failed to settle a block watch observation; it is visited again"
            );
        }
        recorded
    }
}

/// Retry delay of an observation after `attempts` unsuccessful visits:
/// the re-check delay, doubling per attempt, at most an hour.
fn observation_backoff(recheck: Duration, attempts: u32) -> Duration {
    recheck
        .saturating_mul(2u32.saturating_pow(attempts.min(16)))
        .min(MAX_OBSERVATION_BACKOFF)
}

/// When the next visit may start: the earliest of the rotation's next
/// turn, the earliest due re-check and the earliest due observation, but
/// never closer than `min_spacing` to the last visit.
fn next_wake(
    rotation_turn: Instant,
    recheck_due: Option<Instant>,
    observation_due: Option<Instant>,
    last_visit: Option<Instant>,
    min_spacing: Duration,
) -> Instant {
    let due = [recheck_due, observation_due]
        .into_iter()
        .flatten()
        .fold(rotation_turn, Instant::min);
    last_visit.map_or(due, |last| due.max(last + min_spacing))
}

/// The kinds of visit the loop takes turns between, in turn order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Recheck,
    Observation,
    Rotation,
}

const TURN_ORDER: [Turn; 3] = [Turn::Recheck, Turn::Observation, Turn::Rotation];

/// The first due kind after `last` in turn order, so every due kind is
/// served within three visits whatever the others do.
fn pick_turn(last: Turn, due: impl Fn(Turn) -> bool) -> Option<Turn> {
    let start = TURN_ORDER
        .iter()
        .position(|turn| *turn == last)
        .unwrap_or(0)
        + 1;
    (0..TURN_ORDER.len())
        .map(|offset| TURN_ORDER[(start + offset) % TURN_ORDER.len()])
        .find(|turn| due(*turn))
}

/// Drive the paced loop while the lease is held. Returns when the lease
/// is lost (renewal cancelled the token).
async fn hold_lease(
    sweeper: &ReleaseSweeper,
    config: &ReleaseSweepConfig,
    rotation: &mut Rotation,
    cancel: &CancellationToken,
    observations: Option<&ObservationQueue>,
) {
    let mut last_visit: Option<Instant> = None;
    let mut last_turn = Turn::Rotation;

    loop {
        if observations.is_some_and(|queue| queue.signals.take_rotation_request()) {
            rotation.request(Instant::now());
        }
        let observation_due = match observations {
            Some(queue) => queue.next_due().await,
            None => None,
        };
        let wake = next_wake(
            rotation.next_turn,
            sweeper.sweep_state().next_recheck_due(),
            observation_due,
            last_visit,
            config.min_spacing(),
        );
        let woken = async {
            match observations {
                Some(queue) => queue.signals.wake.notified().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = sleep_until(wake) => {}
            // New observations or a rotation request: recompute the wake.
            _ = woken => continue,
        }

        // One visit per turn; due kinds take turns (see `pick_turn`).
        let now = Instant::now();
        let recheck_due = sweeper
            .sweep_state()
            .next_recheck_due()
            .is_some_and(|due| due <= now);
        let observation_is_due = observation_due.is_some_and(|due| due <= now);
        let rotation_due = now >= rotation.next_turn;
        let Some(turn) = pick_turn(last_turn, |turn| match turn {
            Turn::Recheck => recheck_due,
            Turn::Observation => observation_is_due,
            Turn::Rotation => rotation_due,
        }) else {
            continue;
        };
        match turn {
            Turn::Recheck => {
                if let Some(account_id) = sweeper.sweep_state().pop_due_recheck(now) {
                    sweeper.visit_absorbing(&account_id).await;
                    last_visit = Some(now);
                }
            }
            Turn::Observation => {
                if let Some(queue) = observations {
                    queue.visit_next(sweeper).await;
                }
                // Counted against the rate floor even when nothing could
                // be visited (the store failed between its two reads), so
                // a store that keeps reporting a due observation it cannot
                // hand out never spins the loop.
                last_visit = Some(now);
            }
            Turn::Rotation => match rotation.step(sweeper, config, now).await {
                Ok(Step::Visited) => last_visit = Some(now),
                Ok(Step::Idle) => {}
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Release sweep rotation step failed; backing off"
                    );
                    rotation.next_turn = now + config.recheck();
                }
            },
        }
        last_turn = turn;
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
/// size instead of the old one. With the rotation disabled
/// (`GUARDIAN_RELEASE_SWEEP_ENABLED=false`) no walk is scheduled, but a
/// walk block watch requests still runs.
struct Rotation {
    enabled: bool,
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
    /// Whether the current walk was requested by block watch.
    requested: bool,
    /// A walk was requested while one was in progress: another starts as
    /// soon as it completes.
    requested_after_current: bool,
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
        let first_start = if config.enabled { now } else { now + NEVER };
        Self {
            enabled: config.enabled,
            rotation: config.rotation(),
            min_spacing: config.min_spacing(),
            page_size: config.page_size.max(1),
            spacing: config.min_spacing(),
            buffer: VecDeque::new(),
            cursor: None,
            started_at: None,
            requested: false,
            requested_after_current: false,
            next_start: first_start,
            next_turn: first_start,
            visited: 0,
            unchecked: 0,
        }
    }

    /// Block watch skipped blocks: run one walk that starts no earlier
    /// than `now` — now when idle, right after the current walk otherwise
    /// (accounts it already visited may have moved in the skipped blocks).
    fn request(&mut self, now: Instant) {
        if self.started_at.is_some() {
            self.requested_after_current = true;
        } else {
            self.requested = true;
            if self.next_start > now {
                self.next_start = now;
                self.next_turn = now;
            }
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
                    requested_by_block_watch = self.requested,
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
            requested_by_block_watch = self.requested,
            "Release sweep rotation completed"
        );
        self.cursor = None;
        self.buffer.clear();
        self.requested = std::mem::take(&mut self.requested_after_current);
        self.next_start = if self.requested {
            Instant::now()
        } else if self.enabled {
            started_at + config.rotation()
        } else {
            Instant::now() + NEVER
        };
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
        assert_eq!(
            next_wake(rotation_turn, None, None, None, floor),
            rotation_turn
        );
        // An earlier re-check wins...
        let recheck = now + Duration::from_secs(5);
        assert_eq!(
            next_wake(rotation_turn, Some(recheck), None, None, floor),
            recheck
        );
        // ...and so does an earlier observation...
        let observation = now + Duration::from_secs(2);
        assert_eq!(
            next_wake(rotation_turn, Some(recheck), Some(observation), None, floor),
            observation
        );
        // ...but never closer than the rate floor to the last visit.
        assert_eq!(
            next_wake(rotation_turn, Some(now), Some(now), Some(now), floor),
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
        fn get_state_head(
            &self,
            _: &str,
            _: &serde_json::Value,
        ) -> std::result::Result<crate::state_object::StateHead, String> {
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
        async fn observe_commitment(
            &self,
            account_id: &str,
            expected_commitment: &str,
            read_mode: crate::network::RpcReadMode,
        ) -> std::result::Result<crate::network::ObservedState, String> {
            let verification = self
                .verify_commitment(account_id, expected_commitment, read_mode)
                .await?;
            Ok(crate::network::ObservedState {
                verification,
                block: 0,
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
        ) -> std::result::Result<crate::network::AppliedState, String> {
            unimplemented!()
        }
        fn verify_and_apply_delta(
            &self,
            _: &str,
            _: &serde_json::Value,
            _: &serde_json::Value,
        ) -> std::result::Result<crate::network::AppliedState, String> {
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
        fn validate_first_state(&self, _: &serde_json::Value) -> std::result::Result<(), String> {
            unimplemented!()
        }
        fn validate_multisig_auth(&self, _: &serde_json::Value) -> std::result::Result<(), String> {
            unimplemented!()
        }
        fn validate_guardian_commitment(
            &self,
            _: &serde_json::Value,
            _: &str,
            _: guardian_shared::SignatureScheme,
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
            nonce: None,
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
        hold_lease(&sweeper, &config, &mut rotation, &cancel, None).await;
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

    // --- Block watch observations (issue #585) -----------------------------

    use crate::builder::clock::test::MockClock;
    use crate::coordination::{InMemoryBlockWatchStore, NewObservation};

    fn observation_queue(
        store: Arc<InMemoryBlockWatchStore>,
        clock: Arc<dyn Clock>,
        max_attempts: u32,
    ) -> ObservationQueue {
        ObservationQueue {
            store,
            signals: Arc::new(BlockWatchSignals::default()),
            clock,
            recheck: Duration::from_secs(60),
            max_attempts,
        }
    }

    fn observed(account_id: &str, block_num: u32) -> NewObservation {
        NewObservation {
            account_id: account_id.to_string(),
            block_num,
            on_chain_commitment: "0xstored".to_string(),
        }
    }

    #[test]
    fn observation_retries_double_from_the_recheck_delay_to_an_hour() {
        let recheck = Duration::from_secs(60);
        let waits: Vec<u64> = (0..8)
            .map(|attempts| observation_backoff(recheck, attempts).as_secs())
            .collect();
        assert_eq!(waits, vec![60, 120, 240, 480, 960, 1920, 3600, 3600]);
        assert_eq!(
            observation_backoff(recheck, u32::MAX),
            MAX_OBSERVATION_BACKOFF
        );
    }

    #[tokio::test]
    async fn a_checked_observation_is_deleted_and_an_unchecked_one_retried_until_dropped() {
        let store = Arc::new(InMemoryBlockWatchStore::new());
        let clock = MockClock::fixed("2026-11-02T12:00:00Z");
        let queue = observation_queue(store.clone(), Arc::new(clock.clone()), 3);
        let now = clock.now();
        store
            .record_observations(&[observed("0xa", 1), observed("0xb", 2)], now)
            .await
            .unwrap();

        let first = store.next_due_observation(now).await.unwrap().unwrap();
        assert_eq!(
            queue.settle(&first, VisitOutcome::Checked).await,
            BlockWatchVisitOutcome::Checked
        );
        let second = store.next_due_observation(now).await.unwrap().unwrap();
        assert_eq!(second.account_id, "0xb");
        assert_eq!(
            queue.settle(&second, VisitOutcome::Deferred).await,
            BlockWatchVisitOutcome::Deferred
        );
        assert_eq!(store.next_due_observation(now).await.unwrap(), None);
        assert_eq!(
            store.next_observation_due_at().await.unwrap(),
            Some(now + chrono::Duration::seconds(60)),
            "retried after the re-check delay"
        );
        clock.advance_secs(60);
        let retried = store
            .next_due_observation(clock.now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retried.attempts, 1);
        assert_eq!(
            queue.settle(&retried, VisitOutcome::Failed).await,
            BlockWatchVisitOutcome::Failed
        );
        assert_eq!(
            store.next_observation_due_at().await.unwrap(),
            Some(clock.now() + chrono::Duration::seconds(120)),
            "the retry delay doubles"
        );
        clock.advance_secs(120);
        let last = store
            .next_due_observation(clock.now())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(last.attempts, 2);
        assert_eq!(
            queue.settle(&last, VisitOutcome::Deferred).await,
            BlockWatchVisitOutcome::Dropped,
            "the third unchecked visit is the last"
        );
        assert_eq!(store.count_pending_observations().await.unwrap(), 0);
    }

    /// Run `hold_lease` for `window` of virtual time over the three-account
    /// rotation of `run_loop`, with `observed` accounts queued as block
    /// watch observations of their stored state (each checks in one
    /// visit). Returns the visited account ids in order (every visit reads
    /// its row by id) and the queue.
    async fn run_loop_with_observations(
        config: ReleaseSweepConfig,
        observed_accounts: &[&str],
        request_rotation: bool,
        window: Duration,
    ) -> (Vec<String>, Arc<InMemoryBlockWatchStore>) {
        let mut storage = MockStorageBackend::new();
        for _ in 0..2_000 {
            storage = storage.with_pull_state(Ok(state_row()));
        }
        let mut metadata = MockMetadataStore::new()
            .with_count_release_sweep_accounts(Ok(3))
            .with_list_release_sweep_ids(Ok(vec![]))
            .with_list_release_sweep_ids(Ok(vec!["a".into(), "b".into(), "c".into()]));
        for id in ["a", "b", "c"].iter().chain(observed_accounts) {
            metadata = metadata.with_row(miden_meta(id));
        }
        let metadata = Arc::new(metadata);
        let network = Arc::new(SlowNetwork {
            probe_delay: Duration::ZERO,
            probes: StdMutex::new(vec![]),
        });
        let state = create_test_app_state_with_mocks(Arc::new(storage), network, metadata.clone());
        let sweeper =
            ReleaseSweeper::new(state, 2, config.recheck(), Arc::new(SweepState::default()));
        let store = Arc::new(InMemoryBlockWatchStore::new());
        let clock: Arc<dyn Clock> = Arc::new(crate::builder::clock::SystemClock);
        let queue = observation_queue(store.clone(), clock.clone(), 20);
        let observations: Vec<NewObservation> = observed_accounts
            .iter()
            .enumerate()
            .map(|(index, id)| observed(id, 100 + index as u32))
            .collect();
        store
            .record_observations(&observations, clock.now())
            .await
            .unwrap();
        if request_rotation {
            queue.signals.request_rotation();
        }

        let cancel = CancellationToken::new();
        let mut rotation = Rotation::new(&config);
        let canceller = {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                sleep(window).await;
                cancel.cancel();
            })
        };
        hold_lease(&sweeper, &config, &mut rotation, &cancel, Some(&queue)).await;
        canceller.await.unwrap();
        let visits = metadata.get_calls.lock().unwrap().clone();
        (visits, store)
    }

    #[tokio::test(start_paused = true)]
    async fn observations_take_turns_with_the_rotation_and_are_drained_at_the_rate_floor() {
        let config = ReleaseSweepConfig::default()
            .with_rotation_seconds(300)
            .with_max_rate_per_second(5)
            .with_page_size(10);
        let observed = ["o1", "o2", "o3", "o4", "o5", "o6"];
        let (visits, store) =
            run_loop_with_observations(config, &observed, false, Duration::from_secs(10)).await;
        assert_eq!(
            visits,
            vec!["o1", "a", "o2", "o3", "o4", "o5", "o6"],
            "an observation, the rotation's due turn, then the rest of the queue"
        );
        assert_eq!(
            store.count_pending_observations().await.unwrap(),
            0,
            "every checked observation is deleted"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_of_observations_never_raises_the_visit_rate() {
        let config = ReleaseSweepConfig::default()
            .with_rotation_seconds(300)
            .with_max_rate_per_second(5)
            .with_page_size(10);
        let observed: Vec<String> = (0..200).map(|n| format!("obs{n:03}")).collect();
        let refs: Vec<&str> = observed.iter().map(String::as_str).collect();
        let (visits, store) =
            run_loop_with_observations(config, &refs, false, Duration::from_secs(10)).await;
        // 5 visits per second over 10 s, plus the visit at t = 0.
        assert!(visits.len() <= 51, "{} visits in 10 s", visits.len());
        assert!(
            visits.len() >= 45,
            "the queue kept draining: {}",
            visits.len()
        );
        assert_eq!(
            store.count_pending_observations().await.unwrap(),
            200 - visits.iter().filter(|id| id.starts_with("obs")).count() as u64
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_requested_walk_runs_once_even_with_the_rotation_disabled() {
        let config = ReleaseSweepConfig::default()
            .with_enabled(false)
            .with_rotation_seconds(300)
            .with_page_size(10);
        let (visits, _) =
            run_loop_with_observations(config.clone(), &[], true, Duration::from_secs(3_600)).await;
        assert_eq!(visits, vec!["a", "b", "c"], "one walk, then none");

        let (visits, _) =
            run_loop_with_observations(config, &[], false, Duration::from_secs(3_600)).await;
        assert!(visits.is_empty(), "no walk without a request: {visits:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_walk_requested_mid_walk_starts_again_once_it_completes() {
        let config = ReleaseSweepConfig::default()
            .with_rotation_seconds(300)
            .with_page_size(10);
        let mut rotation = Rotation::new(&config);
        let start = Instant::now();
        rotation.request(start);
        assert_eq!(rotation.next_start, start, "idle: now");
        rotation.started_at = Some(start);
        rotation.request(start + Duration::from_secs(10));
        assert!(rotation.requested_after_current);
        rotation.complete(&config);
        assert!(
            rotation.next_start <= Instant::now(),
            "the requested walk follows at once instead of after the rotation window"
        );
        assert!(rotation.requested);
        rotation.started_at = Some(Instant::now());
        rotation.complete(&config);
        assert!(!rotation.requested);
        assert!(
            rotation.next_start > Instant::now(),
            "then the schedule resumes"
        );
    }
}
