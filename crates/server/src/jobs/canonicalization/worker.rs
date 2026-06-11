use crate::error::Result;
use crate::state::AppState;
use tokio::time::interval;

use super::processor::{DeltasProcessor, Processor, TestDeltasProcessor};

pub fn start_worker(state: AppState) {
    tokio::spawn(async move {
        run_worker(state).await;
    });
}

async fn run_worker(state: AppState) {
    let config = match &state.canonicalization {
        Some(config) => config.clone(),
        None => {
            tracing::warn!(
                "Canonicalization worker started in optimistic mode - this should not happen"
            );
            return;
        }
    };

    let processor = DeltasProcessor::new(state.clone(), config.clone());
    let mut interval_timer = interval(config.check_interval());

    loop {
        interval_timer.tick().await;

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

        if let Err(e) = result {
            tracing::error!(error = %e, "Canonicalization worker error");
        }
    }
}

pub async fn process_all_accounts_now(state: &AppState) -> Result<()> {
    let processor = TestDeltasProcessor::new(state.clone());
    processor.process_all_accounts().await
}
