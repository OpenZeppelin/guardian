//! The release sweep task: one lease holder, a slow paced walk of the
//! fleet (one rotation per `rotation_seconds`, never faster than
//! `max_rate_per_second`), and a short-cadence hot pass over the accounts
//! that need attention sooner. The cursor advances per visited account,
//! so a lost lease or a slow node never skips accounts — the walk resumes
//! where it stopped.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, MissedTickBehavior, interval, sleep, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::coordination::LeaderElector;
use crate::error::Result;
use crate::jobs::canonicalization::spawn_lease_renewal;
use crate::metadata::AccountMetadata;
use crate::metrics::labels::RunOutcome;
use crate::release_sweep::ReleaseSweepConfig;
use crate::state::AppState;

use super::sweep::{ReleaseSweeper, SweepState, SweepSummary};

/// Lease TTL / renew cadence: the holder renews every third of the TTL,
/// like the canonicalization worker, so one missed renewal never loses
/// the lease.
const LEASE_TTL: Duration = Duration::from_secs(30);
const LEASE_RENEW_INTERVAL: Duration = Duration::from_secs(10);

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
    let sweep_state = Arc::new(SweepState::default());
    let mut rotation = Rotation::new(&config);

    loop {
        let lease = match leader.try_acquire(LEASE_TTL).await {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                sleep(config.hot_interval()).await;
                continue;
            }
            Err(error) => {
                tracing::warn!(error = %error, "Failed to acquire the release sweep lease");
                sleep(config.hot_interval()).await;
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
        );
        let sweeper = ReleaseSweeper::new(
            state.clone(),
            config.confirmations,
            sweep_state.clone(),
            cancel.clone(),
        );

        hold_lease(&sweeper, &config, &mut rotation, &cancel).await;

        cancel.cancel();
        let _ = renewal.await;
        if let Err(error) = leader.release(lease).await {
            tracing::warn!(error = %error, "Failed to release the release sweep lease");
        }
        tracing::info!("Release sweep lease released; retrying acquisition");
        sleep(config.hot_interval()).await;
    }
}

/// Drive the hot timer and the paced rotation while the lease is held.
/// Returns when the lease is lost (renewal cancelled the token).
async fn hold_lease(
    sweeper: &ReleaseSweeper,
    config: &ReleaseSweepConfig,
    rotation: &mut Rotation,
    cancel: &CancellationToken,
) {
    let mut hot_timer = interval(config.hot_interval());
    hot_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut next_account_at = Instant::now();

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = hot_timer.tick() => {
                run_hot_pass(sweeper).await;
            }
            _ = sleep_until(next_account_at) => {
                next_account_at = match rotation.step(sweeper, config).await {
                    Ok(Step::Visited) => Instant::now() + rotation.spacing,
                    Ok(Step::Idle { until }) => until,
                    Err(error) => {
                        tracing::warn!(
                            error = %error,
                            "Release sweep rotation step failed; backing off"
                        );
                        Instant::now() + config.hot_interval()
                    }
                };
            }
        }
    }
}

async fn run_hot_pass(sweeper: &ReleaseSweeper) {
    let started = std::time::Instant::now();
    let result = sweeper.hot_pass().await;
    let outcome = match &result {
        Ok(summary) if summary.cancelled => RunOutcome::Cancelled,
        Ok(summary) if summary.failed_accounts > 0 => RunOutcome::Partial,
        Ok(_) => RunOutcome::Completed,
        Err(_) => RunOutcome::Error,
    };
    metrics::counter!(
        crate::metrics::names::RELEASE_SWEEP_HOT_PASSES_TOTAL,
        crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
    )
    .increment(1);
    match result {
        Ok(summary) if summary.accounts > 0 => tracing::debug!(
            accounts = summary.accounts,
            failed_accounts = summary.failed_accounts,
            duration_seconds = started.elapsed().as_secs_f64(),
            "Release sweep hot pass completed"
        ),
        Ok(_) => {}
        Err(error) => tracing::error!(error = %error, "Release sweep hot pass failed"),
    }
}

/// Outcome of one rotation step.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// One account was visited; the next is due after the pacing gap.
    Visited,
    /// Nothing to do until `until` (the current rotation is complete, or
    /// the fleet is empty).
    Idle { until: Instant },
}

/// The paced walk of the fleet. Pacing spreads one walk over
/// `rotation_seconds` (spacing = rotation / fleet size), never tighter
/// than `min_spacing()`, so a small fleet finishes early and idles while
/// a large one is bounded by the RPC rate.
struct Rotation {
    rotation: Duration,
    min_spacing: Duration,
    page_size: u32,
    spacing: Duration,
    buffer: VecDeque<AccountMetadata>,
    started_at: Option<Instant>,
    next_start: Instant,
    visited: usize,
    failed: usize,
}

impl Rotation {
    fn new(config: &ReleaseSweepConfig) -> Self {
        Self {
            rotation: config.rotation(),
            min_spacing: config.min_spacing(),
            page_size: config.page_size.max(1),
            spacing: config.min_spacing(),
            buffer: VecDeque::new(),
            started_at: None,
            next_start: Instant::now(),
            visited: 0,
            failed: 0,
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
    ) -> Result<Step> {
        if self.started_at.is_none() {
            let now = Instant::now();
            if now < self.next_start {
                return Ok(Step::Idle {
                    until: self.next_start,
                });
            }
            let fleet_size = sweeper.fleet_size().await?;
            self.spacing = self.spacing_for(fleet_size);
            self.started_at = Some(now);
            self.visited = 0;
            self.failed = 0;
            tracing::info!(
                fleet_size,
                spacing_ms = self.spacing.as_millis(),
                rotation_seconds = config.rotation_seconds,
                resumed_from_cursor = sweeper.sweep_state().cursor().is_some(),
                "Release sweep rotation started"
            );
        }

        if self.buffer.is_empty() {
            let cursor = sweeper.sweep_state().cursor();
            let page = sweeper.list_page(cursor.as_deref(), self.page_size).await?;
            if page.is_empty() {
                return Ok(self.complete(sweeper, config));
            }
            self.buffer.extend(page);
        }

        let metadata = self
            .buffer
            .pop_front()
            .expect("buffer refilled above or the rotation completed");
        let failed = sweeper.visit_absorbing(&metadata).await;
        // Advance the cursor only past the account actually visited, so
        // a lost lease or a failed page mid-walk never skips accounts.
        sweeper.sweep_state().set_cursor(Some(metadata.account_id));
        self.visited += 1;
        self.failed += usize::from(failed);
        Ok(Step::Visited)
    }

    fn complete(&mut self, sweeper: &ReleaseSweeper, config: &ReleaseSweepConfig) -> Step {
        let started_at = self.started_at.take().unwrap_or_else(Instant::now);
        let elapsed = started_at.elapsed();
        let summary = SweepSummary {
            accounts: self.visited,
            failed_accounts: self.failed,
            cancelled: sweeper.is_cancelled(),
        };
        let outcome = if summary.cancelled {
            RunOutcome::Cancelled
        } else if summary.failed_accounts > 0 {
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
            accounts = summary.accounts,
            failed_accounts = summary.failed_accounts,
            duration_seconds = elapsed.as_secs_f64(),
            "Release sweep rotation completed"
        );
        sweeper.sweep_state().set_cursor(None);
        self.buffer.clear();
        self.next_start = started_at + config.rotation();
        Step::Idle {
            until: self.next_start,
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::metadata::auth::Auth;
    use crate::metadata::{AccountMetadata, NetworkConfig};
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};

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
                Arc::new(SweepState::default()),
                CancellationToken::new(),
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
    async fn rotation_advances_the_cursor_per_visited_account_and_idles_until_the_next_start() {
        // Three accounts, page size 2, rotation 300 s: each step visits
        // one account and parks the cursor on it; the fourth step finds
        // the walk complete and idles until start + rotation.
        let config = ReleaseSweepConfig::default()
            .with_rotation_seconds(300)
            .with_page_size(2);
        let (sweeper, metadata) = sweeper(
            MockMetadataStore::new()
                .with_list(Ok(vec!["0xa".into(), "0xb".into(), "0xc".into()]))
                // page 3 (popped last): empty
                .with_list_release_sweep_page(Ok(vec![]))
                // page 2
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xc")]))
                // page 1
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa"), miden_meta("0xb")])),
        );
        let mut rotation = Rotation::new(&config);
        let start = Instant::now();

        assert_eq!(
            rotation.step(&sweeper, &config).await.unwrap(),
            Step::Visited
        );
        assert_eq!(sweeper.sweep_state().cursor().as_deref(), Some("0xa"));
        assert_eq!(
            rotation.spacing,
            Duration::from_secs(100),
            "300 s over 3 accounts"
        );
        assert_eq!(
            rotation.step(&sweeper, &config).await.unwrap(),
            Step::Visited
        );
        assert_eq!(sweeper.sweep_state().cursor().as_deref(), Some("0xb"));
        assert_eq!(
            rotation.step(&sweeper, &config).await.unwrap(),
            Step::Visited
        );
        assert_eq!(sweeper.sweep_state().cursor().as_deref(), Some("0xc"));

        let idle = rotation.step(&sweeper, &config).await.unwrap();
        assert_eq!(
            idle,
            Step::Idle {
                until: start + Duration::from_secs(300)
            }
        );
        assert_eq!(
            sweeper.sweep_state().cursor(),
            None,
            "a completed walk resets the cursor"
        );
        assert_eq!(
            metadata.get_list_release_sweep_page_calls(),
            vec![(None, 2), (Some("0xb".into()), 2), (Some("0xc".into()), 2)]
        );

        // Before the next start the rotation stays idle without listing.
        assert!(matches!(
            rotation.step(&sweeper, &config).await.unwrap(),
            Step::Idle { .. }
        ));
        assert_eq!(metadata.get_list_release_sweep_page_calls().len(), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_page_keeps_the_cursor_so_the_walk_resumes() {
        let config = ReleaseSweepConfig::default().with_page_size(1);
        let (sweeper, _metadata) = sweeper(
            MockMetadataStore::new()
                .with_list(Ok(vec!["0xa".into(), "0xb".into()]))
                .with_list_release_sweep_page(Err("store down".into()))
                .with_list_release_sweep_page(Ok(vec![miden_meta("0xa")])),
        );
        let mut rotation = Rotation::new(&config);
        assert_eq!(
            rotation.step(&sweeper, &config).await.unwrap(),
            Step::Visited
        );
        assert!(rotation.step(&sweeper, &config).await.is_err());
        assert_eq!(
            sweeper.sweep_state().cursor().as_deref(),
            Some("0xa"),
            "the cursor stays on the last visited account"
        );
        assert!(
            rotation.started_at.is_some(),
            "the rotation is still in progress"
        );
    }
}
