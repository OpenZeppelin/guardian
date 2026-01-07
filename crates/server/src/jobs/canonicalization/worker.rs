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

        if let Err(e) = processor.process_all_accounts().await {
            tracing::error!(error = %e, "Canonicalization worker error");
        }
    }
}

pub async fn process_all_accounts_now(state: &AppState) -> Result<()> {
    let processor = TestDeltasProcessor::new(state.clone());
    processor.process_all_accounts().await
}
