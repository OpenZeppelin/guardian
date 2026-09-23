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
//! Every cross-account aggregate (`accounts_by_auth_method`,
//! `delta_status_counts`, `in_flight_proposal_count`, `latest_activity`,
//! and the total once available) is served from the published
//! `/dashboard/stats` snapshot (issue #371), so the overview reports one
//! consistent `aggregates_as_of` and can never contradict
//! `/dashboard/stats`. The walk that builds the snapshot applies the
//! FR-029 filesystem inventory threshold and marks any aggregate it
//! declined or failed to compute; those names surface here in
//! `degraded_aggregates`. Until the first publication after startup the
//! snapshot-served aggregates are reported degraded rather than as zeros.
//! This deliberately trades freshness (Postgres used to compute the delta
//! and proposal aggregates live) for a consistent cached overview.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::build_info;
use crate::error::{GuardianError, Result};
use crate::state::AppState;

/// Stable label for a degraded cross-account aggregate. Surfaced in
/// `DashboardInfoResponse.degraded_aggregates` so dashboard clients
/// can branch on the specific aggregate rather than the wall-clock
/// service status.
pub const AGG_DELTA_STATUS_COUNTS: &str = "delta_status_counts";
pub const AGG_IN_FLIGHT_PROPOSAL_COUNT: &str = "in_flight_proposal_count";
pub const AGG_LATEST_ACTIVITY: &str = "latest_activity";
pub const AGG_ACCOUNTS_BY_AUTH_METHOD: &str = "accounts_by_auth_method";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DashboardServiceStatus {
    Healthy,
    Degraded,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardDeltaStatusCounts {
    pub candidate: u64,
    pub canonical: u64,
    pub retained: u64,
    pub discarded: u64,
}

/// Build identity for the running `guardian-server` binary. Values are
/// stable for the lifetime of the process; surfaced so operators can
/// confirm which version/SHA is responding without reading logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardBuildInfo {
    /// `CARGO_PKG_VERSION` from `guardian-server`.
    pub version: &'static str,
    /// Short git SHA at build time. `"unknown"` when neither
    /// `GUARDIAN_GIT_SHA` nor a working tree git repo were available
    /// to `build.rs`.
    pub git_commit: &'static str,
    /// `"debug"` or `"release"` based on `cfg!(debug_assertions)`.
    pub profile: &'static str,
    /// Wall-clock time the server initialized its dashboard state.
    pub started_at: String,
}

/// Per-account-method canonicalization fan-in configuration. `None`
/// means the server is running in optimistic mode (deltas are written
/// directly as canonical and never enter the candidate state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardCanonicalizationConfig {
    pub check_interval_seconds: u64,
    pub max_retries: u32,
    pub submission_grace_period_seconds: u64,
    /// How long retry-exhausted candidates are kept as `retained` for
    /// background reconciliation (issue #345). `0` = retention disabled.
    pub retained_ttl_seconds: u64,
    /// Cadence of the dedicated reconcile pass over recoverable deltas.
    /// Individual accounts back off further as their rows age, so a
    /// retained row being reconsidered less often than this is expected.
    pub reconcile_interval_seconds: u64,
    /// Accounts one reconcile pass visits at most (rotation cursor).
    pub reconcile_page_size: u32,
}

/// Backend configuration snapshot. Stable for the lifetime of the
/// process; lets operators distinguish a filesystem dev box from a
/// postgres-backed prod replica without inspecting environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardBackendInfo {
    /// `"filesystem"` or `"postgres"` from the cargo feature flag.
    pub storage: &'static str,
    /// Acknowledgement signature schemes wired into the server's
    /// `AckRegistry`. Stable order (alphabetic) so clients can rely on
    /// the listing.
    pub supported_ack_schemes: Vec<&'static str>,
    /// `None` when running in optimistic-commit mode; `Some(_)` when
    /// the canonicalization worker is active.
    pub canonicalization: Option<DashboardCanonicalizationConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardInfoResponse {
    pub service_status: DashboardServiceStatus,
    pub environment: String,
    pub build: DashboardBuildInfo,
    pub backend: DashboardBackendInfo,
    /// Total registered accounts. Once the `/dashboard/stats` snapshot
    /// is available this is `accounts.total` from that snapshot, so it
    /// always equals the sum of `accounts_by_auth_method` and matches
    /// `/dashboard/stats`; before the first publication it is a live
    /// count.
    pub total_account_count: u64,
    /// RFC3339 time the published snapshot that backs every
    /// cross-account aggregate below was computed; `null` until the
    /// first publication after startup (all of them are then listed in
    /// `degraded_aggregates`). Same value as `/dashboard/stats.as_of`.
    pub aggregates_as_of: Option<String>,
    /// Counts of accounts grouped by stable `Auth::method_label()`.
    /// Keys never collide with internal enum names. Served from the
    /// same snapshot as `GET /dashboard/stats` (`accounts.by_auth_method`),
    /// so the two endpoints agree; empty and listed in
    /// `degraded_aggregates` only until that snapshot first publishes
    /// after startup.
    pub accounts_by_auth_method: BTreeMap<String, u64>,
    /// Greater of the most recent delta status timestamp and the most
    /// recent proposal originating timestamp across all accounts as of
    /// `aggregates_as_of`; `None` (serialized as `null`) when the
    /// inventory has produced no activity yet, OR when this aggregate
    /// is degraded.
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
///   - [`GuardianError::StorageError`] if no snapshot is published yet
///     and the fallback account-count read fails. Aggregates the walk
///     declined are `degraded_aggregates` entries, never errors.
pub async fn get_dashboard_info(state: &AppState) -> Result<DashboardInfoResponse> {
    // Every cross-account aggregate comes from the published snapshot;
    // the live inventory count is read only while no snapshot exists yet,
    // so a metadata-store blip never fails the overview once one is
    // published and steady-state requests do no inventory reads.
    let snapshot = state.dashboard.stats().current();
    let total_account_count = match &snapshot {
        Some(snapshot) => snapshot.accounts.total,
        None => state
            .metadata
            .list()
            .await
            .map_err(|e| {
                GuardianError::StorageError(format!("Failed to list account metadata: {e}"))
            })?
            .len() as u64,
    };

    // Storage label reflects the *runtime* backend selected by the
    // builder, not the cargo feature the binary was compiled with — a
    // postgres-capable build that's running against the filesystem
    // backend would mislead operators if we reported the feature
    // flag. Dispatch via `state.storage.kind()`.
    let storage_label = match state.storage.kind() {
        crate::storage::StorageType::Filesystem => "filesystem",
        crate::storage::StorageType::Postgres => "postgres",
    };
    let backend = DashboardBackendInfo {
        storage: storage_label,
        supported_ack_schemes: vec!["ecdsa", "falcon"],
        canonicalization: state.canonicalization.as_ref().map(|c| {
            DashboardCanonicalizationConfig {
                check_interval_seconds: c.check_interval_seconds,
                max_retries: c.max_retries,
                submission_grace_period_seconds: c.submission_grace_period_seconds,
                retained_ttl_seconds: c.retained_ttl_seconds,
                reconcile_interval_seconds: c.reconcile_interval_seconds,
                reconcile_page_size: c.reconcile_page_size,
            }
        }),
    };

    let build = DashboardBuildInfo {
        version: build_info::VERSION,
        git_commit: build_info::GIT_SHA,
        profile: build_info::build_profile(),
        started_at: state.dashboard.started_at().to_rfc3339(),
    };

    let mut response = DashboardInfoResponse {
        service_status: DashboardServiceStatus::Healthy,
        environment: state.dashboard.environment().to_string(),
        build,
        backend,
        total_account_count,
        aggregates_as_of: None,
        accounts_by_auth_method: BTreeMap::new(),
        latest_activity: None,
        delta_status_counts: DashboardDeltaStatusCounts::default(),
        in_flight_proposal_count: 0,
        degraded_aggregates: Vec::new(),
    };

    // Issue #371: every cross-account aggregate is served from the
    // published `/dashboard/stats` snapshot, so the overview reports one
    // consistent `aggregates_as_of` and never contradicts
    // `/dashboard/stats`. Until the first publication after startup the
    // snapshot-served aggregates are reported degraded (never as zeros);
    // the live account count is still returned. An aggregate the walk
    // itself declined (filesystem inventory threshold, storage failure)
    // carries its stable name in `degraded_aggregates`.
    match snapshot {
        Some(snapshot) => {
            response.aggregates_as_of = Some(snapshot.as_of.to_rfc3339());
            response.accounts_by_auth_method = snapshot.accounts.by_auth_method.clone();
            let inventory = &snapshot.inventory;
            if let Some(counts) = inventory.delta_status_counts {
                response.delta_status_counts = DashboardDeltaStatusCounts {
                    candidate: counts.candidate,
                    canonical: counts.canonical,
                    retained: counts.retained,
                    discarded: counts.discarded,
                };
            }
            if let Some(in_flight) = inventory.in_flight_proposal_count {
                response.in_flight_proposal_count = in_flight;
            }
            response.latest_activity = inventory.latest_activity.map(|dt| dt.to_rfc3339());
            if !inventory.degraded.is_empty() {
                response.service_status = DashboardServiceStatus::Degraded;
                response
                    .degraded_aggregates
                    .extend(inventory.degraded.iter().cloned());
            }
        }
        None => {
            response.service_status = DashboardServiceStatus::Degraded;
            response.degraded_aggregates.extend([
                AGG_ACCOUNTS_BY_AUTH_METHOD.to_string(),
                AGG_DELTA_STATUS_COUNTS.to_string(),
                AGG_IN_FLIGHT_PROPOSAL_COUNT.to_string(),
                AGG_LATEST_ACTIVITY.to_string(),
            ]);
        }
    }

    Ok(response)
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::dashboard::stats::{
        AccountLifecycle, AccountStatsRecord, InventoryAggregates, SnapshotDeltaCounts,
        StatsSnapshot, VaultOutcome,
    };
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;

    async fn build_state(account_ids: Vec<String>, storage: MockStorageBackend) -> AppState {
        let metadata_store = MockMetadataStore::new().with_list(Ok(account_ids));
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata_store),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::fixed("2026-09-15T12:00:00Z")),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    fn records(
        composition: &[(&str, usize)],
        updated_at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<AccountStatsRecord> {
        let mut out = Vec::new();
        for (auth_method, count) in composition {
            for i in 0..*count {
                out.push(AccountStatsRecord {
                    account_id: format!("{auth_method}-{i}"),
                    updated_at: Some(updated_at),
                    auth_method: auth_method.to_string(),
                    authorized_signer_count: 1,
                    lifecycle: AccountLifecycle::Active,
                    state_commitment: None,
                    vault: VaultOutcome::NotApplicable,
                });
            }
        }
        out
    }

    /// Publish a `/dashboard/stats` snapshot into the replica cache with
    /// the given account composition and inventory aggregates.
    fn publish_stats_snapshot(
        state: &AppState,
        composition: &[(&str, usize)],
        inventory: InventoryAggregates,
    ) {
        let now = state.clock.now();
        state.dashboard.stats().publish(Arc::new(StatsSnapshot::new(
            7,
            now,
            now,
            records(composition, now),
            inventory,
        )));
    }

    fn available(
        counts: SnapshotDeltaCounts,
        in_flight: u64,
        latest: Option<chrono::DateTime<chrono::Utc>>,
    ) -> InventoryAggregates {
        InventoryAggregates::available(counts, in_flight, latest)
    }

    #[tokio::test]
    async fn every_cross_account_aggregate_is_served_from_the_stats_snapshot() {
        // Once a snapshot exists the live inventory is not consulted at
        // all: a failing metadata store must not fail the overview.
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        let state = AppState {
            storage: Arc::new(MockStorageBackend::new()),
            metadata: Arc::new(
                MockMetadataStore::new().with_list(Err("metadata store unreachable".into())),
            ),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::fixed("2026-09-15T12:00:00Z")),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };
        let latest = state.clock.now() - chrono::Duration::minutes(3);
        publish_stats_snapshot(
            &state,
            &[("miden_falcon", 2), ("miden_ecdsa", 1)],
            available(
                SnapshotDeltaCounts {
                    candidate: 5,
                    canonical: 100,
                    retained: 3,
                    discarded: 2,
                },
                4,
                Some(latest),
            ),
        );

        let info = get_dashboard_info(&state).await.unwrap();
        // The total comes from the snapshot (3), never the live list, so
        // it always equals the sum of the per-method counts.
        assert_eq!(info.total_account_count, 3);
        assert_eq!(info.accounts_by_auth_method.get("miden_falcon"), Some(&2));
        assert_eq!(info.accounts_by_auth_method.get("miden_ecdsa"), Some(&1));
        assert_eq!(info.delta_status_counts.candidate, 5);
        assert_eq!(info.delta_status_counts.canonical, 100);
        assert_eq!(info.delta_status_counts.retained, 3);
        assert_eq!(info.delta_status_counts.discarded, 2);
        assert_eq!(info.in_flight_proposal_count, 4);
        assert_eq!(info.latest_activity, Some(latest.to_rfc3339()));
        assert_eq!(info.aggregates_as_of, Some(state.clock.now().to_rfc3339()));
        assert_eq!(info.service_status, DashboardServiceStatus::Healthy);
        assert!(info.degraded_aggregates.is_empty());
    }

    #[tokio::test]
    async fn snapshot_served_aggregates_are_degraded_until_first_publication() {
        // Nothing published yet (process just started, or the shared
        // store is empty). Every snapshot-served aggregate is reported
        // degraded — never as an empty map or zero masquerading as data —
        // while the live account count is still returned.
        let state = build_state(
            vec!["a".to_string(), "b".to_string()],
            MockStorageBackend::new(),
        )
        .await;

        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 2);
        assert_eq!(info.aggregates_as_of, None);
        assert_eq!(info.service_status, DashboardServiceStatus::Degraded);
        assert_eq!(
            info.degraded_aggregates,
            vec![
                AGG_ACCOUNTS_BY_AUTH_METHOD.to_string(),
                AGG_DELTA_STATUS_COUNTS.to_string(),
                AGG_IN_FLIGHT_PROPOSAL_COUNT.to_string(),
                AGG_LATEST_ACTIVITY.to_string(),
            ]
        );
        assert!(info.accounts_by_auth_method.is_empty());
        assert!(info.latest_activity.is_none());
    }

    #[tokio::test]
    async fn aggregates_the_walk_declined_are_reported_by_stable_name() {
        // The walk marks what it could not compute (filesystem inventory
        // threshold, a failed storage aggregate); info relays those
        // names and keeps the aggregates it did get.
        let state = build_state(vec!["a".to_string()], MockStorageBackend::new()).await;
        let inventory = InventoryAggregates {
            delta_status_counts: None,
            in_flight_proposal_count: Some(2),
            latest_activity: None,
            degraded: vec![
                AGG_DELTA_STATUS_COUNTS.to_string(),
                AGG_LATEST_ACTIVITY.to_string(),
            ],
        };
        publish_stats_snapshot(&state, &[("miden_falcon", 1)], inventory);

        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 1);
        assert_eq!(info.accounts_by_auth_method.get("miden_falcon"), Some(&1));
        assert_eq!(info.in_flight_proposal_count, 2);
        assert_eq!(info.service_status, DashboardServiceStatus::Degraded);
        assert_eq!(
            info.degraded_aggregates,
            vec![
                AGG_DELTA_STATUS_COUNTS.to_string(),
                AGG_LATEST_ACTIVITY.to_string()
            ]
        );
        assert!(
            !info
                .degraded_aggregates
                .iter()
                .any(|s| s == AGG_ACCOUNTS_BY_AUTH_METHOD)
        );
    }

    #[tokio::test]
    async fn empty_inventory_returns_explicit_zeros_and_no_activity() {
        let state = build_state(Vec::new(), MockStorageBackend::new()).await;
        publish_stats_snapshot(
            &state,
            &[],
            available(SnapshotDeltaCounts::default(), 0, None),
        );
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
    async fn build_info_fields_are_populated_from_compile_time_constants() {
        let state = build_state(Vec::new(), MockStorageBackend::new()).await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.build.version, build_info::VERSION);
        assert_eq!(info.build.git_commit, build_info::GIT_SHA);
        assert!(info.build.profile == "debug" || info.build.profile == "release");
        assert!(chrono::DateTime::parse_from_rfc3339(&info.build.started_at).is_ok());
    }

    #[tokio::test]
    async fn backend_storage_label_reflects_runtime_storage_kind() {
        let state = build_state(
            Vec::new(),
            MockStorageBackend::new().with_kind(crate::storage::StorageType::Postgres),
        )
        .await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.backend.storage, "postgres");
        assert_eq!(info.backend.supported_ack_schemes, vec!["ecdsa", "falcon"]);
    }

    #[tokio::test]
    async fn backend_storage_label_reports_filesystem_when_storage_kind_is_filesystem() {
        let state = build_state(
            Vec::new(),
            MockStorageBackend::new().with_kind(crate::storage::StorageType::Filesystem),
        )
        .await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.backend.storage, "filesystem");
    }

    #[tokio::test]
    async fn canonicalization_is_none_when_disabled_in_state() {
        let state = build_state(Vec::new(), MockStorageBackend::new()).await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert!(info.backend.canonicalization.is_none());
    }

    #[tokio::test]
    async fn canonicalization_is_populated_from_app_state_config() {
        let mut state = build_state(Vec::new(), MockStorageBackend::new()).await;
        state.canonicalization = Some(
            crate::canonicalization::CanonicalizationConfig::new(30, 5)
                .with_submission_grace_period_seconds(120),
        );
        let info = get_dashboard_info(&state).await.unwrap();
        let canon = info
            .backend
            .canonicalization
            .expect("canonicalization config");
        assert_eq!(canon.check_interval_seconds, 30);
        assert_eq!(canon.max_retries, 5);
        assert_eq!(canon.submission_grace_period_seconds, 120);
    }

    #[tokio::test]
    async fn environment_comes_from_dashboard_state_default() {
        let state = build_state(Vec::new(), MockStorageBackend::new()).await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.environment, "testnet");
    }
}
