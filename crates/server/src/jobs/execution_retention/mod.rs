//! Deletes finished execution attempts once they outlive `GUARDIAN_EXECUTION_RECORD_RETENTION_DAYS`.
//! Storage decides what is prunable and never touches an active attempt or the newest attempt
//! of a proposal it still holds, so every replica may sweep: batches are idempotent and two
//! sweeps running at once only split the work.

use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::state::AppState;
use crate::storage::StorageBackend;

/// How often each replica sweeps.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long after startup the first sweep runs, so it stays clear of the startup work.
pub const FIRST_SWEEP_DELAY: Duration = Duration::from_secs(5 * 60);
/// Attempts one storage call deletes at most, bounding each transaction.
pub const SWEEP_BATCH: usize = 1_000;

/// One replica's retention sweep over the execution records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionSweep {
    retention: chrono::Duration,
    batch: usize,
}

impl RetentionSweep {
    pub fn new(retention_days: u32) -> Self {
        Self {
            retention: chrono::Duration::days(i64::from(retention_days)),
            batch: SWEEP_BATCH,
        }
    }

    /// Deletes batches until one comes back short, and reports how many attempts went.
    pub async fn run(
        &self,
        storage: &dyn StorageBackend,
        now: DateTime<Utc>,
    ) -> Result<usize, String> {
        let cutoff = now - self.retention;
        let mut pruned = 0;
        loop {
            let batch = storage.prune_execution_records(cutoff, self.batch).await?;
            crate::metrics::execution::record_records_pruned(batch);
            pruned += batch;
            if batch < self.batch {
                return Ok(pruned);
            }
        }
    }
}

/// Runs whenever the execution reconciler does, since both read the same records.
pub fn start_execution_retention_sweep(state: AppState, retention_days: u32) {
    tokio::spawn(async move {
        let sweep = RetentionSweep::new(retention_days);
        tokio::time::sleep(FIRST_SWEEP_DELAY).await;
        loop {
            match sweep.run(state.storage.as_ref(), state.clock.now()).await {
                Ok(pruned) => tracing::info!(
                    pruned,
                    retention_days,
                    "execution record retention sweep finished"
                ),
                Err(error) => {
                    tracing::warn!(%error, "execution record retention sweep failed")
                }
            }
            tokio::time::sleep(SWEEP_INTERVAL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mocks::MockStorageBackend;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-08T12:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn a_sweep_deletes_batches_until_one_comes_back_short() {
        let storage = MockStorageBackend::new()
            .with_prune_execution_records(Ok(3))
            .with_prune_execution_records(Ok(SWEEP_BATCH))
            .with_prune_execution_records(Ok(SWEEP_BATCH));
        let pruned = RetentionSweep::new(30).run(&storage, now()).await.unwrap();
        assert_eq!(pruned, 2 * SWEEP_BATCH + 3);
        let calls = storage
            .prune_execution_records_calls
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            calls,
            vec![(now() - chrono::Duration::days(30), SWEEP_BATCH); 3],
            "every batch uses the same cutoff, retention days before the run"
        );
    }

    #[tokio::test]
    async fn an_empty_store_takes_one_batch() {
        let storage = MockStorageBackend::new();
        assert_eq!(RetentionSweep::new(2).run(&storage, now()).await, Ok(0));
        assert_eq!(
            storage.prune_execution_records_calls.lock().unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn a_failing_batch_stops_the_sweep() {
        let storage = MockStorageBackend::new()
            .with_prune_execution_records(Ok(1))
            .with_prune_execution_records(Err("storage down".to_string()))
            .with_prune_execution_records(Ok(SWEEP_BATCH));
        assert_eq!(
            RetentionSweep::new(30).run(&storage, now()).await,
            Err("storage down".to_string())
        );
        assert_eq!(
            storage.prune_execution_records_calls.lock().unwrap().len(),
            2
        );
    }
}
