//! Dashboard inventory and lifecycle health summary endpoint service.
//!
//! Spec reference: `005-operator-dashboard-metrics` FR-008..FR-012, US2.
//!
//! Returns a single point-in-time snapshot of:
//!   - service status (healthy / degraded depending on partial-source failures)
//!   - deployment environment identifier
//!   - total configured account count
//!   - latest activity timestamp (max of delta + proposal status timestamps)
//!   - delta lifecycle counts (`candidate` / `canonical` / `discarded`)
//!   - in-flight (Pending) proposal count
//!   - which aggregates were marked degraded
//!
//! Aggregates that fan out across all accounts (`delta_status_counts`,
//! `in_flight_proposal_count`, `latest_activity`) are short-circuited
//! to a degraded marker when the configured filesystem threshold is
//! exceeded, per FR-029. Total account count is always returned (cheap
//! single-call to the metadata store).
//!
//! Per the v1 Miden-oriented scope, `GROUP BY` and `MAX` aggregates are
//! computed via service-layer fan-out using the existing
//! `pull_deltas_after` and `pull_pending_proposals` storage trait
//! methods. A future feature can promote `delta.status` /
//! `status_timestamp` to typed indexed columns for native SQL
//! aggregates if profiling under real load shows pain (research.md
//! Decision 1).

use serde::Serialize;

use crate::error::{GuardianError, Result};
use crate::state::AppState;

/// Stable label for a degraded cross-account aggregate. Surfaced in
/// `DashboardInfoResponse.degraded_aggregates` so dashboard clients
/// can branch on the specific aggregate rather than the wall-clock
/// service status.
pub const AGG_DELTA_STATUS_COUNTS: &str = "delta_status_counts";
pub const AGG_IN_FLIGHT_PROPOSAL_COUNT: &str = "in_flight_proposal_count";
pub const AGG_LATEST_ACTIVITY: &str = "latest_activity";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardServiceStatus {
    Healthy,
    Degraded,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DashboardDeltaStatusCounts {
    pub candidate: u64,
    pub canonical: u64,
    pub discarded: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardInfoResponse {
    pub service_status: DashboardServiceStatus,
    pub environment: String,
    pub total_account_count: u64,
    /// Greater of the most recent delta status timestamp and the most
    /// recent proposal originating timestamp across all accounts;
    /// `None` (serialized as `null`) when the inventory has produced
    /// no activity yet, OR when this aggregate is degraded.
    pub latest_activity: Option<String>,
    pub delta_status_counts: DashboardDeltaStatusCounts,
    pub in_flight_proposal_count: u64,
    /// Names of aggregates that returned a degraded marker on this
    /// response. Stable strings — clients branch on these to decide
    /// whether to retry or rely on the partial value.
    pub degraded_aggregates: Vec<String>,
}

/// Compute the dashboard info snapshot.
///
/// Errors:
///   - [`GuardianError::StorageError`] if even the cheap account-count
///     read fails. Per-aggregate fan-out failures are downgraded to
///     `degraded_aggregates` entries rather than failing the whole
///     response.
pub async fn get_dashboard_info(state: &AppState) -> Result<DashboardInfoResponse> {
    let account_ids = state.metadata.list().await.map_err(|e| {
        GuardianError::StorageError(format!("Failed to list account metadata: {e}"))
    })?;
    let total_account_count = account_ids.len() as u64;

    let mut response = DashboardInfoResponse {
        service_status: DashboardServiceStatus::Healthy,
        environment: state.dashboard.environment().to_string(),
        total_account_count,
        latest_activity: None,
        delta_status_counts: DashboardDeltaStatusCounts::default(),
        in_flight_proposal_count: 0,
        degraded_aggregates: Vec::new(),
    };

    // FR-029: filesystem-only threshold. Postgres serves these
    // aggregates from indexed `GROUP BY` / `MAX` queries and is not
    // bounded by inventory size. Above-threshold filesystem
    // deployments mark the fan-out aggregates as degraded and skip
    // the scan; total account count is always reported.
    if state.storage.kind() == crate::storage::StorageType::Filesystem {
        let threshold = state.dashboard.filesystem_aggregate_threshold();
        if account_ids.len() > threshold {
            response.service_status = DashboardServiceStatus::Degraded;
            response.degraded_aggregates.extend([
                AGG_DELTA_STATUS_COUNTS.to_string(),
                AGG_IN_FLIGHT_PROPOSAL_COUNT.to_string(),
                AGG_LATEST_ACTIVITY.to_string(),
            ]);
            return Ok(response);
        }
    }

    // Push aggregates down to the storage layer. Postgres serves them
    // as indexed `GROUP BY` / `MAX` queries; filesystem fans out as
    // before but encapsulates the logic. Any per-aggregate failure is
    // marked degraded rather than failing the whole response.
    match state.storage.count_deltas_by_status().await {
        Ok(counts) => {
            response.delta_status_counts.candidate = counts.candidate;
            response.delta_status_counts.canonical = counts.canonical;
            response.delta_status_counts.discarded = counts.discarded;
        }
        Err(e) => {
            tracing::warn!(error = %e, "dashboard info: count_deltas_by_status failed");
            response.service_status = DashboardServiceStatus::Degraded;
            response
                .degraded_aggregates
                .push(AGG_DELTA_STATUS_COUNTS.to_string());
        }
    }

    match state.storage.count_in_flight_proposals().await {
        Ok(n) => {
            response.in_flight_proposal_count = n;
        }
        Err(e) => {
            tracing::warn!(error = %e, "dashboard info: count_in_flight_proposals failed");
            response.service_status = DashboardServiceStatus::Degraded;
            response
                .degraded_aggregates
                .push(AGG_IN_FLIGHT_PROPOSAL_COUNT.to_string());
        }
    }

    match state.storage.latest_activity_timestamp().await {
        Ok(ts) => {
            response.latest_activity = ts.map(|dt| dt.to_rfc3339());
        }
        Err(e) => {
            tracing::warn!(error = %e, "dashboard info: latest_activity_timestamp failed");
            response.service_status = DashboardServiceStatus::Degraded;
            response
                .degraded_aggregates
                .push(AGG_LATEST_ACTIVITY.to_string());
        }
    }

    Ok(response)
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::testing::mocks::{MockMetadataStore, MockStorageBackend};
    use std::sync::Arc;

    /// Build an `AppState` whose dashboard aggregate trait calls are
    /// each pre-stubbed. The new architecture (Decision 1, revised)
    /// has the storage layer own `count_deltas_by_status`,
    /// `count_in_flight_proposals`, and `latest_activity_timestamp`,
    /// so the service-layer test simply queues stubbed responses.
    #[allow(clippy::too_many_arguments)]
    async fn build_state(
        account_ids: Vec<String>,
        delta_counts: crate::storage::DeltaStatusCounts,
        in_flight_proposals: u64,
        latest_activity: Option<chrono::DateTime<chrono::Utc>>,
    ) -> AppState {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let metadata_store = MockMetadataStore::new().with_list(Ok(account_ids));
        let storage = MockStorageBackend::new()
            .with_count_deltas_by_status(Ok(delta_counts))
            .with_count_in_flight_proposals(Ok(in_flight_proposals))
            .with_latest_activity_timestamp(Ok(latest_activity));

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");

        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    #[tokio::test]
    async fn empty_inventory_returns_explicit_zeros_and_no_activity() {
        let state = build_state(
            Vec::new(),
            crate::storage::DeltaStatusCounts::default(),
            0,
            None,
        )
        .await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 0);
        assert_eq!(info.delta_status_counts.candidate, 0);
        assert_eq!(info.delta_status_counts.canonical, 0);
        assert_eq!(info.delta_status_counts.discarded, 0);
        assert_eq!(info.in_flight_proposal_count, 0);
        assert!(info.latest_activity.is_none());
        assert_eq!(info.service_status, DashboardServiceStatus::Healthy);
        assert!(info.degraded_aggregates.is_empty());
    }

    #[tokio::test]
    async fn aggregates_propagate_storage_response_into_wire_shape() {
        let state = build_state(
            vec!["0xa".into(), "0xb".into()],
            crate::storage::DeltaStatusCounts {
                candidate: 1,
                canonical: 1,
                discarded: 1,
            },
            2,
            chrono::DateTime::parse_from_rfc3339("2026-05-09T11:00:00Z")
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc)),
        )
        .await;

        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 2);
        assert_eq!(info.delta_status_counts.candidate, 1);
        assert_eq!(info.delta_status_counts.canonical, 1);
        assert_eq!(info.delta_status_counts.discarded, 1);
        assert_eq!(info.in_flight_proposal_count, 2);
        assert_eq!(
            info.latest_activity.as_deref(),
            Some("2026-05-09T11:00:00+00:00")
        );
    }

    #[tokio::test]
    async fn environment_comes_from_dashboard_state_default() {
        let state = build_state(
            Vec::new(),
            crate::storage::DeltaStatusCounts::default(),
            0,
            None,
        )
        .await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.environment, "testnet");
    }

    #[tokio::test]
    async fn delta_read_failure_marks_status_counts_degraded_but_keeps_total() {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let metadata = MockMetadataStore::new().with_list(Ok(vec!["0xa".into()]));
        // count_deltas_by_status fails; the other two aggregates
        // succeed. Service should mark only the affected aggregate as
        // degraded.
        let storage = MockStorageBackend::new()
            .with_count_deltas_by_status(Err("boom".into()))
            .with_count_in_flight_proposals(Ok(0))
            .with_latest_activity_timestamp(Ok(None));

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");

        let state = AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };

        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 1);
        assert_eq!(info.service_status, DashboardServiceStatus::Degraded);
        assert!(
            info.degraded_aggregates
                .iter()
                .any(|s| s == AGG_DELTA_STATUS_COUNTS)
        );
    }

    #[tokio::test]
    async fn above_filesystem_threshold_marks_fanout_aggregates_degraded() {
        use crate::dashboard::DashboardState;

        // Build a state with 3 accounts but a threshold of 1 — the
        // service must short-circuit fan-out aggregates to degraded.
        let mut config = crate::dashboard::DashboardConfig::for_tests();
        // Hack: we can't reach private fields from outside the module,
        // but we can construct DashboardState through for_tests + then
        // test by ensuring our default threshold of 1000 means 1001
        // accounts trigger it. However that's a lot of test data, so
        // we just simulate with a custom DashboardState if possible.
        let _ = &mut config;
        // Use the default threshold (1000); seed 1001 account IDs.
        let account_ids: Vec<String> = (0..1001).map(|i| format!("acc{i}")).collect();
        // We don't need pull_deltas to succeed — the threshold check
        // returns before that.
        let metadata = MockMetadataStore::new().with_list(Ok(account_ids.clone()));
        // Threshold check is filesystem-only; flag the mock as
        // Filesystem so the FR-029 short-circuit fires.
        let storage = MockStorageBackend::new().with_kind(crate::storage::StorageType::Filesystem);
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        let state = AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 1001);
        assert_eq!(info.service_status, DashboardServiceStatus::Degraded);
        assert!(
            info.degraded_aggregates
                .iter()
                .any(|s| s == AGG_DELTA_STATUS_COUNTS)
        );
        assert!(
            info.degraded_aggregates
                .iter()
                .any(|s| s == AGG_IN_FLIGHT_PROPOSAL_COUNT)
        );
        assert!(
            info.degraded_aggregates
                .iter()
                .any(|s| s == AGG_LATEST_ACTIVITY)
        );
        // Counts are zero because we didn't fan out.
        assert_eq!(info.delta_status_counts.candidate, 0);
        assert_eq!(info.in_flight_proposal_count, 0);
    }
}
