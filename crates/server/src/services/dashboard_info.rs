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
//! single-call to the metadata store). `accounts_by_auth_method` is
//! served from the background-maintained `/dashboard/stats` aggregate
//! (issue #371 FR-9) — never threshold-gated, and never contradicting
//! `/dashboard/stats` because both read the same snapshot; it is
//! degraded only until the first refresh has published.
//!
//! Per the v1 Miden-oriented scope, `GROUP BY` and `MAX` aggregates are
//! computed via service-layer fan-out using the existing
//! `pull_deltas_after` and `pull_pending_proposals` storage trait
//! methods. A future feature can promote `delta.status` /
//! `status_timestamp` to typed indexed columns for native SQL
//! aggregates if profiling under real load shows pain (research.md
//! Decision 1).

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
    /// has published this is `accounts.total` from that snapshot, so it
    /// always equals the sum of `accounts_by_auth_method` and matches
    /// `/dashboard/stats`; before the first refresh it is a live count.
    pub total_account_count: u64,
    /// Counts of accounts grouped by stable `Auth::method_label()`.
    /// Keys never collide with internal enum names. Served from the
    /// same snapshot as `GET /dashboard/stats` (`accounts.by_auth_method`),
    /// so the two endpoints agree; empty and listed in
    /// `degraded_aggregates` only until that snapshot first publishes
    /// after startup.
    pub accounts_by_auth_method: BTreeMap<String, u64>,
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
        accounts_by_auth_method: BTreeMap::new(),
        latest_activity: None,
        delta_status_counts: DashboardDeltaStatusCounts::default(),
        in_flight_proposal_count: 0,
        degraded_aggregates: Vec::new(),
    };

    // accounts_by_auth_method: read from the background-maintained
    // `/dashboard/stats` snapshot (issue #371 FR-9). This replaces the
    // former N-point-read fan-out and its inventory threshold: the
    // aggregate is bucketed once per refresh regardless of inventory
    // size, and `/dashboard/info` and `/dashboard/stats` cannot
    // disagree because they read the same snapshot — including the
    // total, which is taken from the snapshot too so it always equals
    // the sum of the per-method counts. The only degraded window is
    // between process start and the first published refresh.
    match state.dashboard.stats().current() {
        Some(snapshot) => {
            response.total_account_count = snapshot.accounts.total;
            response.accounts_by_auth_method = snapshot.accounts.by_auth_method.clone();
        }
        None => {
            response.service_status = DashboardServiceStatus::Degraded;
            response
                .degraded_aggregates
                .push(AGG_ACCOUNTS_BY_AUTH_METHOD.to_string());
        }
    }

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
            response.delta_status_counts.retained = counts.retained;
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
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    /// Publish a `/dashboard/stats` snapshot with the given
    /// `(auth_method, count)` composition so `/dashboard/info` can
    /// serve `accounts_by_auth_method` from it (issue #371 FR-9).
    fn publish_stats_snapshot(state: &AppState, composition: &[(&'static str, usize)]) {
        use crate::dashboard::stats::{
            AccountLifecycle, AccountStatsRecord, StatsSnapshot, VaultOutcome,
        };
        let mut records = Vec::new();
        for (auth_method, count) in composition {
            for i in 0..*count {
                records.push(AccountStatsRecord {
                    account_id: format!("{auth_method}-{i}"),
                    updated_at: Some(state.clock.now()),
                    auth_method,
                    authorized_signer_count: 1,
                    lifecycle: AccountLifecycle::Active,
                    state_commitment: None,
                    vault: VaultOutcome::NotApplicable,
                });
            }
        }
        state
            .dashboard
            .stats()
            .publish(Arc::new(StatsSnapshot::new(state.clock.now(), records)));
    }

    #[tokio::test]
    async fn accounts_by_auth_method_is_served_from_stats_snapshot() {
        let state = build_state(
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
            Default::default(),
            0,
            None,
        )
        .await;
        publish_stats_snapshot(&state, &[("miden_falcon", 2), ("miden_ecdsa", 1)]);

        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 3);
        assert_eq!(info.accounts_by_auth_method.get("miden_falcon"), Some(&2));
        assert_eq!(info.accounts_by_auth_method.get("miden_ecdsa"), Some(&1));
        assert_eq!(info.service_status, DashboardServiceStatus::Healthy);
        // The aggregate is not degraded on the happy path.
        assert!(
            !info
                .degraded_aggregates
                .iter()
                .any(|s| s == AGG_ACCOUNTS_BY_AUTH_METHOD)
        );
    }

    #[tokio::test]
    async fn accounts_by_auth_method_marks_degraded_until_first_stats_refresh() {
        // No snapshot published yet (process just started). The
        // aggregate is reported degraded — never as an empty map
        // masquerading as "no accounts" — without failing the response
        // or degrading the other aggregates.
        let state = build_state(
            vec!["a".to_string(), "b".to_string()],
            Default::default(),
            0,
            None,
        )
        .await;

        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 2);
        assert_eq!(info.service_status, DashboardServiceStatus::Degraded);
        assert_eq!(
            info.degraded_aggregates,
            vec![AGG_ACCOUNTS_BY_AUTH_METHOD.to_string()]
        );
        // Counts map left empty when the aggregate is marked degraded.
        assert!(info.accounts_by_auth_method.is_empty());
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
        // An empty inventory still needs a published (empty) stats
        // snapshot for the auth-method aggregate to count as computed.
        publish_stats_snapshot(&state, &[]);
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
                retained: 1,
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
        assert_eq!(info.delta_status_counts.retained, 1);
        assert_eq!(info.delta_status_counts.discarded, 1);
        assert_eq!(info.in_flight_proposal_count, 2);
        assert_eq!(
            info.latest_activity.as_deref(),
            Some("2026-05-09T11:00:00+00:00")
        );
    }

    #[tokio::test]
    async fn build_info_fields_are_populated_from_compile_time_constants() {
        let state = build_state(
            Vec::new(),
            crate::storage::DeltaStatusCounts::default(),
            0,
            None,
        )
        .await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.build.version, env!("CARGO_PKG_VERSION"));
        assert!(!info.build.git_commit.is_empty());
        assert!(info.build.profile == "debug" || info.build.profile == "release");
        assert!(chrono::DateTime::parse_from_rfc3339(&info.build.started_at).is_ok());
    }

    #[tokio::test]
    async fn backend_storage_label_reflects_runtime_storage_kind() {
        // `MockStorageBackend` defaults to `StorageType::Postgres`
        // unless explicitly overridden via `with_kind`. The dashboard
        // info handler reports the *runtime* kind via
        // `state.storage.kind()`, not the compiled cargo feature, so
        // this test exercises the postgres-label path regardless of
        // whether the binary was built with `--features postgres`.
        let state = build_state(
            Vec::new(),
            crate::storage::DeltaStatusCounts::default(),
            0,
            None,
        )
        .await;
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.backend.storage, "postgres");
        assert!(info.backend.supported_ack_schemes.contains(&"falcon"));
        assert!(info.backend.supported_ack_schemes.contains(&"ecdsa"));
    }

    #[tokio::test]
    async fn backend_storage_label_reports_filesystem_when_storage_kind_is_filesystem() {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;

        // Override the mock to report Filesystem; verifies that the
        // dashboard handler dispatches off the *runtime* storage kind
        // and not a compile-time constant.
        let metadata = MockMetadataStore::new().with_list(Ok(Vec::new()));
        let storage = MockStorageBackend::new()
            .with_kind(crate::storage::StorageType::Filesystem)
            .with_count_deltas_by_status(Ok(Default::default()))
            .with_count_in_flight_proposals(Ok(0))
            .with_latest_activity_timestamp(Ok(None));
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");

        let state = AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.backend.storage, "filesystem");
    }

    #[tokio::test]
    async fn canonicalization_is_none_when_disabled_in_state() {
        let state = build_state(
            Vec::new(),
            crate::storage::DeltaStatusCounts::default(),
            0,
            None,
        )
        .await;
        // build_state leaves canonicalization = None.
        let info = get_dashboard_info(&state).await.unwrap();
        assert!(info.backend.canonicalization.is_none());
    }

    #[tokio::test]
    async fn canonicalization_is_populated_from_app_state_config() {
        let mut state = build_state(
            Vec::new(),
            crate::storage::DeltaStatusCounts::default(),
            0,
            None,
        )
        .await;
        state.canonicalization = Some(crate::canonicalization::CanonicalizationConfig {
            abandon_quarantine_seconds: 15,
            abandon_quarantine_checks: 2,
            check_interval_seconds: 7,
            fast_promotion_enabled: true,
            fast_promotion_interval_seconds: 3,
            fast_promotion_window_seconds: 30,
            max_retries: 13,
            submission_grace_period_seconds: 42,
            divergence_confirmations: 2,
            max_concurrent_accounts: 4,
            retained_ttl_seconds: 86_400,
            reconcile_interval_seconds: 60,
            reconcile_page_size: 100,
        });
        let info = get_dashboard_info(&state).await.unwrap();
        let cfg = info.backend.canonicalization.expect("config present");
        assert_eq!(cfg.check_interval_seconds, 7);
        assert_eq!(cfg.max_retries, 13);
        assert_eq!(cfg.submission_grace_period_seconds, 42);
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
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
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

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        let state = AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };
        // Issue #371 FR-9: the auth-method aggregate comes from the
        // stats snapshot and is NOT subject to the inventory threshold.
        publish_stats_snapshot(&state, &[("miden_falcon", 1001)]);
        let info = get_dashboard_info(&state).await.unwrap();
        assert_eq!(info.total_account_count, 1001);
        assert_eq!(info.service_status, DashboardServiceStatus::Degraded);
        assert_eq!(
            info.accounts_by_auth_method.get("miden_falcon"),
            Some(&1001)
        );
        assert!(
            !info
                .degraded_aggregates
                .iter()
                .any(|s| s == AGG_ACCOUNTS_BY_AUTH_METHOD)
        );
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
