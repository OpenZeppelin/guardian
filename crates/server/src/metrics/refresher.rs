//! Background refresher for slow aggregate gauges.
//!
//! Scrapes must never trigger storage reads, so the cross-account
//! aggregates that `/dashboard/info` computes on demand are computed
//! here on a fixed cadence and published as gauges. A failed refresh
//! leaves the previous gauge values in place ("stale beats absent")
//! and increments a failure counter; the staleness itself is
//! observable as `time() - guardian_metrics_refresh_timestamp_seconds`.
//!
//! Split for testability: [`fetch_snapshot`] is async and pure data,
//! [`apply_snapshot`] is synchronous gauge writes (so tests can scope
//! it under `metrics::with_local_recorder`, which is thread-local and
//! does not follow spawned tasks).

use metrics::{counter, gauge};
use metrics_exporter_prometheus::PrometheusHandle;
use std::sync::Arc;
use std::time::Duration;

use super::names::{
    ACCOUNTS_GAUGE, DELTAS_GAUGE, LABEL_STATUS, METRICS_REFRESH_FAILURES_TOTAL,
    METRICS_REFRESH_TIMESTAMP_SECONDS, PROPOSALS_IN_FLIGHT,
};
use crate::builder::clock::Clock;
use crate::metadata::MetadataStore;
use crate::state::AppState;
use crate::storage::{DeltaStatusCounts, StorageBackend};

/// One round of slow-aggregate values, decoupled from gauge writes.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshSnapshot {
    pub delta_counts: DeltaStatusCounts,
    pub in_flight_proposals: u64,
    pub accounts_total: u64,
    pub fetched_at_unix_seconds: f64,
}

/// Gather the aggregates through the existing storage/metadata count
/// APIs (Postgres serves these from indexed columns; the filesystem
/// backend may refuse above its FR-029 aggregate threshold, which
/// surfaces here as `Err` and leaves the gauges stale).
pub async fn fetch_snapshot(
    storage: &Arc<dyn StorageBackend>,
    metadata: &Arc<dyn MetadataStore>,
    clock: &Arc<dyn Clock>,
) -> Result<RefreshSnapshot, String> {
    let delta_counts = storage.count_deltas_by_status().await?;
    let in_flight_proposals = storage.count_in_flight_proposals().await?;
    let accounts_total = metadata.list().await?.len() as u64;

    Ok(RefreshSnapshot {
        delta_counts,
        in_flight_proposals,
        accounts_total,
        fetched_at_unix_seconds: clock.now().timestamp() as f64,
    })
}

/// Publish a snapshot to the gauges. Synchronous by design (see
/// module docs).
pub fn apply_snapshot(snapshot: &RefreshSnapshot) {
    gauge!(DELTAS_GAUGE, LABEL_STATUS => "candidate").set(snapshot.delta_counts.candidate as f64);
    gauge!(DELTAS_GAUGE, LABEL_STATUS => "canonical").set(snapshot.delta_counts.canonical as f64);
    gauge!(DELTAS_GAUGE, LABEL_STATUS => "discarded").set(snapshot.delta_counts.discarded as f64);
    gauge!(PROPOSALS_IN_FLIGHT).set(snapshot.in_flight_proposals as f64);
    gauge!(ACCOUNTS_GAUGE).set(snapshot.accounts_total as f64);
    gauge!(METRICS_REFRESH_TIMESTAMP_SECONDS).set(snapshot.fetched_at_unix_seconds);
}

/// Spawn the refresher loop alongside the other background jobs
/// (mirrors `start_canonicalization_worker`).
pub fn start_refresher(state: &AppState, handle: PrometheusHandle, interval: Duration) {
    let storage = state.storage.clone();
    let metadata = state.metadata.clone();
    let clock = state.clock.clone();
    tokio::spawn(run_refresher(storage, metadata, clock, handle, interval));
}

async fn run_refresher(
    storage: Arc<dyn StorageBackend>,
    metadata: Arc<dyn MetadataStore>,
    clock: Arc<dyn Clock>,
    handle: PrometheusHandle,
    interval: Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        // First tick fires immediately, so gauges are populated at
        // startup instead of after one full interval.
        ticker.tick().await;

        // The exporter requires periodic upkeep (draining histogram
        // buckets) when not using its built-in HTTP listener; piggy-
        // back on the refresh cadence.
        handle.run_upkeep();

        match fetch_snapshot(&storage, &metadata, &clock).await {
            Ok(snapshot) => apply_snapshot(&snapshot),
            Err(error) => {
                counter!(METRICS_REFRESH_FAILURES_TOTAL).increment(1);
                tracing::warn!(
                    error = %error,
                    "metrics refresh failed; aggregate gauges left stale"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builder::clock::test::MockClock;
    use crate::metrics::recorder::build_recorder;
    use crate::testing::mocks::{MockMetadataStore, MockStorageBackend};

    fn block_on<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn fetch_snapshot_gathers_counts_and_timestamp() {
        let storage = MockStorageBackend::new();
        storage
            .count_deltas_by_status_responses
            .lock()
            .unwrap()
            .push(Ok(DeltaStatusCounts {
                candidate: 2,
                canonical: 40,
                discarded: 1,
            }));
        storage
            .count_in_flight_proposals_responses
            .lock()
            .unwrap()
            .push(Ok(7));
        let metadata = MockMetadataStore::new().with_list(Ok(vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
        ]));
        let clock = MockClock::fixed("2026-06-10T12:00:00Z");

        let storage: Arc<dyn StorageBackend> = Arc::new(storage);
        let metadata: Arc<dyn MetadataStore> = Arc::new(metadata);
        let clock: Arc<dyn Clock> = Arc::new(clock);

        let snapshot =
            block_on(async { fetch_snapshot(&storage, &metadata, &clock).await.unwrap() });

        assert_eq!(snapshot.delta_counts.candidate, 2);
        assert_eq!(snapshot.delta_counts.canonical, 40);
        assert_eq!(snapshot.delta_counts.discarded, 1);
        assert_eq!(snapshot.in_flight_proposals, 7);
        assert_eq!(snapshot.accounts_total, 3);
        assert_eq!(
            snapshot.fetched_at_unix_seconds,
            MockClock::fixed("2026-06-10T12:00:00Z").now().timestamp() as f64
        );
    }

    #[test]
    fn fetch_snapshot_propagates_storage_errors() {
        let storage = MockStorageBackend::new();
        storage
            .count_deltas_by_status_responses
            .lock()
            .unwrap()
            .push(Err("aggregate threshold exceeded".to_string()));
        let storage: Arc<dyn StorageBackend> = Arc::new(storage);
        let metadata: Arc<dyn MetadataStore> = Arc::new(MockMetadataStore::new());
        let clock: Arc<dyn Clock> = Arc::new(MockClock::default());

        let result = block_on(async { fetch_snapshot(&storage, &metadata, &clock).await });
        assert!(result.is_err());
    }

    #[test]
    fn apply_snapshot_publishes_gauges() {
        let recorder = build_recorder();
        let handle = recorder.handle();
        let snapshot = RefreshSnapshot {
            delta_counts: DeltaStatusCounts {
                candidate: 5,
                canonical: 100,
                discarded: 2,
            },
            in_flight_proposals: 4,
            accounts_total: 12,
            fetched_at_unix_seconds: 1_780_000_000.0,
        };

        metrics::with_local_recorder(&recorder, || apply_snapshot(&snapshot));

        let rendered = handle.render();
        assert!(rendered.contains("guardian_deltas{status=\"candidate\"} 5"));
        assert!(rendered.contains("guardian_deltas{status=\"canonical\"} 100"));
        assert!(rendered.contains("guardian_deltas{status=\"discarded\"} 2"));
        assert!(rendered.contains("guardian_proposals_in_flight 4"));
        assert!(rendered.contains("guardian_accounts 12"));
        assert!(rendered.contains("guardian_metrics_refresh_timestamp_seconds 1780000000"));
    }

    /// Ticking cadence: the loop fetches immediately, then once per
    /// interval. Uses paused tokio time; the mock storage records how
    /// many times the aggregate query ran.
    #[tokio::test(start_paused = true)]
    async fn refresher_ticks_immediately_then_per_interval() {
        let storage = MockStorageBackend::new();
        let calls = storage.count_deltas_by_status_calls.clone();
        let storage: Arc<dyn StorageBackend> = Arc::new(storage);
        let metadata: Arc<dyn MetadataStore> = Arc::new(MockMetadataStore::new());
        let clock: Arc<dyn Clock> = Arc::new(MockClock::default());
        let recorder = build_recorder();

        let task = tokio::spawn(run_refresher(
            storage,
            metadata,
            clock,
            recorder.handle(),
            Duration::from_secs(30),
        ));

        let wait_for = |expected: u64, calls: Arc<std::sync::Mutex<u64>>| async move {
            for _ in 0..100 {
                if *calls.lock().unwrap() == expected {
                    return;
                }
                tokio::task::yield_now().await;
            }
            panic!(
                "refresher did not reach {expected} fetches (got {})",
                *calls.lock().unwrap()
            );
        };

        wait_for(1, calls.clone()).await;

        tokio::time::advance(Duration::from_secs(30)).await;
        wait_for(2, calls.clone()).await;

        tokio::time::advance(Duration::from_secs(60)).await;
        wait_for(3, calls.clone()).await;

        task.abort();
    }
}
