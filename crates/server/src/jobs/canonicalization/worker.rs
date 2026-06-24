use std::sync::Arc;
use std::time::Duration;

use tokio::time::interval;
use tokio_util::sync::CancellationToken;

use crate::coordination::{LeaderElector, Lease};
use crate::error::Result;
use crate::state::AppState;

use super::processor::{DeltasProcessor, Processor, TestDeltasProcessor};

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
    let mut interval_timer = interval(check_interval);

    loop {
        interval_timer.tick().await;

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

        let processor = DeltasProcessor::with_lease(
            state.clone(),
            config.clone(),
            leader.clone(),
            lease,
            cancel.clone(),
        );

        let started = std::time::Instant::now();
        let result = processor.process_all_accounts().await;
        metrics::histogram!(crate::metrics::names::CANONICALIZATION_RUN_DURATION_SECONDS)
            .record(started.elapsed().as_secs_f64());
        metrics::counter!(
            crate::metrics::names::CANONICALIZATION_RUNS_TOTAL,
            crate::metrics::names::LABEL_OUTCOME =>
                crate::metrics::labels::Outcome::from_ok(result.is_ok()).as_str()
        )
        .increment(1);

        cancel.cancel();
        let _ = renewal.await;

        if let Err(e) = result {
            tracing::error!(error = %e, "Canonicalization worker error");
        }
    }
}

/// Renew the lease on its own timer, concurrent with the pass. On a lost lease
/// (stolen, expired, or store error) it trips `cancel` so the pass aborts at its
/// next checkpoint; the fence check still guards any in-flight write.
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
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = ticker.tick() => match leader.renew(&lease, ttl).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!("Canonicalization lease lost during pass; cancelling");
                        cancel.cancel();
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "Canonicalization lease renew failed; cancelling pass");
                        cancel.cancel();
                        break;
                    }
                },
            }
        }
    })
}

pub async fn process_all_accounts_now(state: &AppState) -> Result<()> {
    let processor = TestDeltasProcessor::new(state.clone());
    processor.process_all_accounts().await
}
