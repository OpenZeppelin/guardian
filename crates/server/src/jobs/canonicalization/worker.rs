use std::sync::Arc;
use std::time::Duration;

use tokio::time::{Instant, Interval, MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use crate::coordination::{LeaderElector, Lease};
use crate::error::Result;
use crate::state::AppState;

use super::processor::{
    DeltasProcessor, FastPassControl, FastPromotionState, PassSummary, ProcessingMode, Processor,
    TestDeltasProcessor,
};

pub fn start_worker(state: AppState, leader: Arc<dyn LeaderElector>) {
    tokio::spawn(async move {
        run_worker(state, leader).await;
    });
}

async fn run_worker(state: AppState, leader: Arc<dyn LeaderElector>) {
    let config = match &state.canonicalization {
        Some(config) => config.clone(),
        None => {
            tracing::warn!(
                "Canonicalization worker started in optimistic mode - this should not happen"
            );
            return;
        }
    };

    let check_interval = config.check_interval();
    // TTL outlives several renew cycles so a healthy holder never loses the lease
    // mid-pass and the lease survives the idle gap between ticks; failover after a
    // crash happens within one TTL.
    let lease_ttl = check_interval * 3;
    let renew_interval = check_interval;
    let mut full_timer = interval(check_interval);
    let mut fast_timer = interval(config.fast_promotion_interval());
    full_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    fast_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let fast_promotion_state = Arc::new(FastPromotionState::default());
    let mut next_full_deadline = Instant::now();

    loop {
        let (mode, scheduled_at) = next_processing_mode(
            &mut full_timer,
            &mut fast_timer,
            config.fast_promotion_enabled,
            config.fast_promotion_window_seconds,
        )
        .await;
        if mode == ProcessingMode::Full {
            next_full_deadline = next_tick_after(scheduled_at, check_interval, Instant::now());
        }
        // First of two reset points: a full tick must defer the fast timer
        // even when the `continue` paths below (not leader, acquire error)
        // never reach the post-pass reset.
        if config.fast_promotion_enabled && mode == ProcessingMode::Full {
            fast_timer.reset();
        }

        let lease = match leader.try_acquire(lease_ttl).await {
            Ok(Some(lease)) => lease,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(error = %error, "Failed to acquire canonicalization lease");
                continue;
            }
        };

        let cancel = CancellationToken::new();
        let renewal = spawn_renewal(
            leader.clone(),
            lease.clone(),
            lease_ttl,
            renew_interval,
            cancel.clone(),
        );

        let fast_promotion_deadline = match mode {
            ProcessingMode::Full => None,
            ProcessingMode::PromoteRecent { .. } => Some(std::cmp::min(
                Instant::now() + config.fast_promotion_interval(),
                next_full_deadline,
            )),
        };
        let processor = DeltasProcessor::with_fast_pass(
            state.clone(),
            config.clone(),
            leader.clone(),
            lease,
            cancel.clone(),
            mode,
            FastPassControl {
                state: fast_promotion_state.clone(),
                deadline: fast_promotion_deadline,
            },
        );

        let started = std::time::Instant::now();
        let result = processor.process_all_accounts().await;
        let outcome = match &result {
            Ok(summary) if summary.cancelled => crate::metrics::labels::RunOutcome::Cancelled,
            Ok(summary) if summary.failed_accounts > 0 => {
                crate::metrics::labels::RunOutcome::Partial
            }
            Ok(_) => crate::metrics::labels::RunOutcome::Completed,
            Err(_) => crate::metrics::labels::RunOutcome::Error,
        };
        let elapsed = started.elapsed().as_secs_f64();
        match mode {
            ProcessingMode::Full => {
                metrics::histogram!(crate::metrics::names::CANONICALIZATION_RUN_DURATION_SECONDS)
                    .record(elapsed);
                metrics::counter!(
                    crate::metrics::names::CANONICALIZATION_RUNS_TOTAL,
                    crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
                )
                .increment(1);
            }
            ProcessingMode::PromoteRecent { .. } => {
                metrics::histogram!(
                    crate::metrics::names::CANONICALIZATION_FAST_RUN_DURATION_SECONDS
                )
                .record(elapsed);
                metrics::counter!(
                    crate::metrics::names::CANONICALIZATION_FAST_RUNS_TOTAL,
                    crate::metrics::names::LABEL_OUTCOME => outcome.as_str()
                )
                .increment(1);
            }
        }

        cancel.cancel();
        let _ = renewal.await;
        // Second reset point: measured from *after* the pass, so a fast
        // tick never fires immediately behind a long full pass.
        if config.fast_promotion_enabled && mode == ProcessingMode::Full {
            fast_timer.reset();
        }

        match result {
            Ok(summary) if summary.cancelled || summary.failed_accounts > 0 => {
                tracing::warn!(
                    accounts = summary.accounts,
                    failed_accounts = summary.failed_accounts,
                    cancelled = summary.cancelled,
                    ?mode,
                    "Canonicalization pass degraded"
                );
            }
            Ok(_) => {}
            Err(e) => {
                tracing::error!(error = %e, ?mode, "Canonicalization worker error");
            }
        }
    }
}

async fn next_processing_mode(
    full_timer: &mut Interval,
    fast_timer: &mut Interval,
    fast_promotion_enabled: bool,
    fast_window_seconds: u64,
) -> (ProcessingMode, Instant) {
    if !fast_promotion_enabled {
        return (ProcessingMode::Full, full_timer.tick().await);
    }

    tokio::select! {
        biased;
        scheduled_at = full_timer.tick() => (ProcessingMode::Full, scheduled_at),
        scheduled_at = fast_timer.tick() => (ProcessingMode::PromoteRecent {
            max_age_seconds: fast_window_seconds,
        }, scheduled_at),
    }
}

fn next_tick_after(mut scheduled_at: Instant, interval: Duration, now: Instant) -> Instant {
    loop {
        scheduled_at += interval;
        if scheduled_at > now {
            return scheduled_at;
        }
    }
}

/// Renew the lease on its own timer, concurrent with the pass. A definitive
/// loss (`Ok(false)`: stolen or expired) trips `cancel` immediately so the pass
/// aborts at its next checkpoint. A store *error* is ambiguous — the lease may
/// well still be held — so one consecutive error is tolerated and retried at
/// the next tick; the second cancels. The margin is guaranteed by construction:
/// `ttl = 3 × renew_interval`, so after one missed renewal the lease (extended
/// at the last successful renew) is still a full interval from expiry, and the
/// fence check guards any in-flight write regardless.
fn spawn_renewal(
    leader: Arc<dyn LeaderElector>,
    lease: Lease,
    ttl: Duration,
    renew_interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = interval(renew_interval);
        ticker.tick().await;
        let mut renew_errors: u32 = 0;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => match leader.renew(&lease, ttl).await {
                    Ok(true) => renew_errors = 0,
                    Ok(false) => {
                        tracing::warn!("Canonicalization lease lost during pass; cancelling");
                        cancel.cancel();
                        break;
                    }
                    Err(error) => {
                        renew_errors += 1;
                        if renew_errors >= 2 {
                            tracing::warn!(error = %error, "Canonicalization lease renew failed again; cancelling pass");
                            cancel.cancel();
                            break;
                        }
                        tracing::warn!(error = %error, "Canonicalization lease renew failed; retrying once before cancelling");
                    }
                },
            }
        }
    })
}

pub async fn process_all_accounts_now(state: &AppState) -> Result<PassSummary> {
    let processor = TestDeltasProcessor::new(state.clone());
    processor.process_all_accounts().await
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::error::GuardianError;
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Elector whose `renew` replays a scripted sequence of outcomes, then
    /// settles on `Ok(true)`.
    struct ScriptedElector {
        renew_outcomes: Mutex<VecDeque<Result<bool>>>,
    }

    impl ScriptedElector {
        fn new(outcomes: impl IntoIterator<Item = Result<bool>>) -> Arc<Self> {
            Arc::new(Self {
                renew_outcomes: Mutex::new(outcomes.into_iter().collect()),
            })
        }
    }

    #[async_trait]
    impl LeaderElector for ScriptedElector {
        async fn try_acquire(&self, _ttl: Duration) -> Result<Option<Lease>> {
            unreachable!("renewal task never acquires")
        }

        async fn renew(&self, _lease: &Lease, _ttl: Duration) -> Result<bool> {
            self.renew_outcomes
                .lock()
                .expect("scripted outcomes lock")
                .pop_front()
                .unwrap_or(Ok(true))
        }

        async fn verify_held(&self, _lease: &Lease) -> Result<bool> {
            Ok(true)
        }

        async fn release(&self, _lease: Lease) -> Result<()> {
            Ok(())
        }

        fn supports_fencing(&self) -> bool {
            false
        }
    }

    fn lease() -> Lease {
        Lease {
            name: "canonicalization".to_string(),
            holder_id: "replica-test".to_string(),
            fence_token: 1,
            expires_at: DateTime::<Utc>::MAX_UTC,
        }
    }

    fn store_error() -> Result<bool> {
        Err(GuardianError::StorageError("store unreachable".to_string()))
    }

    async fn run_ticks(ticks: u32) {
        // Paused-clock advance past `ticks` renew intervals, yielding so the
        // renewal task observes each tick.
        for _ in 0..ticks {
            tokio::time::sleep(Duration::from_secs(11)).await;
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn full_pass_wins_shared_tick() {
        let mut full_timer = interval(Duration::from_secs(10));
        let mut fast_timer = interval(Duration::from_secs(3));
        full_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        fast_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        let initial = next_processing_mode(&mut full_timer, &mut fast_timer, true, 30).await;
        assert_eq!(initial.0, ProcessingMode::Full);
        fast_timer.reset();

        tokio::time::advance(Duration::from_secs(3)).await;
        let fast = next_processing_mode(&mut full_timer, &mut fast_timer, true, 30).await;
        assert_eq!(
            fast.0,
            ProcessingMode::PromoteRecent {
                max_age_seconds: 30
            }
        );

        tokio::time::advance(Duration::from_secs(7)).await;
        let shared_tick = next_processing_mode(&mut full_timer, &mut fast_timer, true, 30).await;
        assert_eq!(shared_tick.0, ProcessingMode::Full);
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_fast_promotion_only_uses_full_timer() {
        let mut full_timer = interval(Duration::from_secs(10));
        let mut fast_timer = interval(Duration::from_secs(3));

        let initial = next_processing_mode(&mut full_timer, &mut fast_timer, false, 30).await;
        assert_eq!(initial.0, ProcessingMode::Full);

        tokio::time::advance(Duration::from_secs(10)).await;
        let next = next_processing_mode(&mut full_timer, &mut fast_timer, false, 30).await;
        assert_eq!(next.0, ProcessingMode::Full);
    }

    #[tokio::test(start_paused = true)]
    async fn next_full_deadline_skips_elapsed_intervals() {
        let scheduled_at = Instant::now();
        tokio::time::advance(Duration::from_secs(25)).await;

        let deadline = next_tick_after(scheduled_at, Duration::from_secs(10), Instant::now());

        assert_eq!(deadline, scheduled_at + Duration::from_secs(30));
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_cancels_immediately_when_lease_definitively_lost() {
        let elector = ScriptedElector::new([Ok(false)]);
        let cancel = CancellationToken::new();
        spawn_renewal(
            elector,
            lease(),
            Duration::from_secs(30),
            Duration::from_secs(10),
            cancel.clone(),
        );

        run_ticks(1).await;
        assert!(
            cancel.is_cancelled(),
            "Ok(false) is a definitive loss and must cancel on the first tick"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_survives_a_single_transient_store_error() {
        let elector = ScriptedElector::new([store_error()]);
        let cancel = CancellationToken::new();
        spawn_renewal(
            elector,
            lease(),
            Duration::from_secs(30),
            Duration::from_secs(10),
            cancel.clone(),
        );

        run_ticks(3).await;
        assert!(
            !cancel.is_cancelled(),
            "one transient renew error must not abort the pass (ttl = 3 × interval leaves margin)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn renewal_cancels_on_second_consecutive_store_error() {
        let elector = ScriptedElector::new([store_error(), store_error()]);
        let cancel = CancellationToken::new();
        spawn_renewal(
            elector,
            lease(),
            Duration::from_secs(30),
            Duration::from_secs(10),
            cancel.clone(),
        );

        run_ticks(2).await;
        assert!(
            cancel.is_cancelled(),
            "two consecutive renew errors must step down before the lease can expire"
        );
    }
}
