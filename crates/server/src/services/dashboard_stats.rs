//! `GET /dashboard/stats` — one-call account and Miden vault aggregates
//! (issue #371).
//!
//! The request path only reads the snapshot that
//! [`crate::dashboard::stats`] maintains in the background: no storage
//! reads, no vault decoding (FR-5 / FR-6). An `updated_since` cutoff is
//! applied to the in-memory per-account records; the unfiltered
//! aggregate is precomputed at refresh time. Staleness is bounded by
//! the refresh interval and always visible through `as_of` (FR-7).
//!
//! Until the first refresh completes the endpoint answers `503`
//! `data_unavailable` (retryable) rather than serving zeros that could
//! be mistaken for an empty inventory (FR-4).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use serde_json::json;

use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::coordination::RefreshRequestOutcome;
use crate::dashboard::AuthenticatedOperator;
use crate::dashboard::stats::{
    AssetAggregate, STATS_REFRESH_COOLDOWN, STATS_REFRESH_STALE_AFTER, SkipReason, StatsSnapshot,
};
use crate::error::{GuardianError, Result};
use crate::state::AppState;

/// Mutually exclusive lifecycle counts: `released` when `released_at`
/// is set, otherwise `paused` when `paused_at` is set, otherwise
/// `active`. Sums to `total`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardLifecycleCounts {
    pub active: u64,
    pub paused: u64,
    pub released: u64,
}

/// Accounts sharing one `(auth_method, authorized_signer_count)` shape.
/// Lets a consumer reproduce its account-shape heuristics without
/// Guardian asserting which client a shape belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardAuthMethodSignerCount {
    /// Stable `Auth::method_label()` (`miden_falcon`, `miden_ecdsa`, `evm`).
    pub auth_method: String,
    /// Distinct authorized signers, as on `DashboardAccountSummary`.
    pub authorized_signer_count: u64,
    pub count: u64,
}

/// Unfiltered account counts (FR-3). `updated_since` does not apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardAccountStats {
    pub total: u64,
    pub by_lifecycle: DashboardLifecycleCounts,
    /// Counts by stable auth-method label. A map because the key is
    /// the closed `Auth::method_label()` vocabulary and it is the very
    /// same aggregate `/dashboard/info.accounts_by_auth_method` serves
    /// (FR-9). Never omitted above an inventory threshold.
    pub by_auth_method: BTreeMap<String, u64>,
    /// Sorted by `(auth_method, authorized_signer_count)`. An array
    /// rather than a nested map because the signer count is an
    /// unbounded integer, which JSON object keys cannot type.
    pub by_auth_method_and_signer_count: Vec<DashboardAuthMethodSignerCount>,
    /// Accounts whose metadata `updated_at` is within the last 7 days,
    /// anchored to `as_of`.
    pub updated_within_7d: u64,
    /// Same, for the last 30 days.
    pub updated_within_30d: u64,
}

/// Base-unit fungible total for one faucet across covered accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardFungibleTotal {
    pub faucet_id: String,
    /// Base-10 decimal string; may exceed `u64` and JavaScript's safe
    /// integer range. No decimals normalization or pricing is applied.
    pub total_amount: String,
}

/// Non-fungible asset count for one faucet across covered accounts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardNonFungibleTotal {
    pub faucet_id: String,
    pub count: u64,
}

/// Asset totals over eligible Miden accounts plus the coverage that
/// qualifies them (FR-1 / FR-4). `covered + sum(skipped) == eligible`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardAssetStats {
    /// Miden accounts passing the `updated_since` filter (EVM accounts
    /// are never eligible — they have no Miden vault).
    pub eligible: u64,
    /// Eligible accounts whose vault was decoded into the totals.
    pub covered: u64,
    /// Eligible accounts not covered, by stable reason. Keys are the
    /// closed [`SkipReason`] vocabulary; a new reason is additive and
    /// `covered + Σskipped == eligible` always holds.
    pub skipped: BTreeMap<SkipReason, u64>,
    /// `true` only when every eligible account is covered. A skipped
    /// account never appears as a zero balance.
    pub complete: bool,
    /// Sorted by `faucet_id`.
    pub fungible: Vec<DashboardFungibleTotal>,
    /// Sorted by `faucet_id`.
    pub non_fungible: Vec<DashboardNonFungibleTotal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardStatsResponse {
    /// RFC3339 time the published aggregate's walk began. Consumers
    /// derive the result's age from this; it is the only truthful age
    /// signal, since a slow or failed walk keeps the previous
    /// publication.
    #[schema(format = DateTime)]
    pub as_of: String,
    /// The applied filter, normalized to RFC3339, or `null` when the
    /// asset aggregate spans every account.
    #[schema(format = DateTime)]
    pub updated_since: Option<String>,
    /// Configured cadence at which the lease holder starts a new walk
    /// (`GUARDIAN_DASHBOARD_STATS_REFRESH_INTERVAL_SECS`). Not a bound
    /// on the age of `as_of`: a slow or failed walk keeps the previous
    /// publication.
    pub refresh_interval_seconds: u64,
    /// Publication counter of the snapshot this response was served
    /// from; identical across replicas for the same publication.
    pub version: i64,
    pub accounts: DashboardAccountStats,
    pub assets: DashboardAssetStats,
}

/// Outcome of an operator-triggered refresh request (`202 Accepted`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DashboardStatsRefreshStatus {
    /// The request is recorded; the lease holder starts a walk on its
    /// next tick. Also returned when a request was already pending.
    Queued,
    /// A walk is already running; no duplicate work was started.
    InProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct DashboardStatsRefreshResponse {
    pub status: DashboardStatsRefreshStatus,
    /// RFC3339 time the pending request was recorded (`queued`).
    pub requested_at: Option<String>,
    /// RFC3339 time the running walk started (`in_progress`).
    pub started_at: Option<String>,
    /// `as_of` of the snapshot currently served, or `null` before the
    /// first publication. Poll `GET /dashboard/stats` for a newer value.
    pub current_as_of: Option<String>,
    /// Minimum seconds between accepted operator requests.
    pub cooldown_seconds: u64,
}

/// Parse the optional `updated_since` query value. Empty / blank is
/// treated as absent (mirrors the global-feed `status` filter); any
/// other non-RFC3339 value is [`GuardianError::InvalidTimestamp`].
pub fn parse_updated_since(raw: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| Some(dt.with_timezone(&Utc)))
        .map_err(|e| {
            GuardianError::InvalidTimestamp(format!(
                "updated_since must be an RFC3339 timestamp, got {raw:?}: {e}"
            ))
        })
}

/// Record an operator request for an out-of-cycle refresh (issue #371).
/// Requires `stats:refresh`. Automatic and operator-triggered refreshes
/// share the lease holder and the control row, so a request never
/// starts duplicate work: while a walk runs the response reports it,
/// and requests within [`STATS_REFRESH_COOLDOWN`] of the last accepted
/// one are refused with `429 rate_limit_exceeded` (`Retry-After` set).
///
/// Errors:
///   - [`GuardianError::RateLimitExceeded`] (`429`) during the cooldown.
///   - [`GuardianError::StorageError`] when the shared control row
///     cannot be read or written.
pub async fn request_dashboard_stats_refresh(
    state: &AppState,
    operator: &AuthenticatedOperator,
    client_ip: Option<String>,
) -> Result<DashboardStatsRefreshResponse> {
    let outcome = state
        .dashboard
        .stats_store()
        .request_refresh(
            &operator.operator_id,
            state.clock.now(),
            STATS_REFRESH_COOLDOWN,
            STATS_REFRESH_STALE_AFTER,
        )
        .await?;
    let current_as_of = state
        .dashboard
        .stats()
        .current()
        .map(|s| s.as_of.to_rfc3339());
    let (status, requested_at, started_at, error) = match outcome {
        RefreshRequestOutcome::Queued { requested_at }
        | RefreshRequestOutcome::AlreadyQueued { requested_at } => (
            DashboardStatsRefreshStatus::Queued,
            Some(requested_at.to_rfc3339()),
            None,
            None,
        ),
        RefreshRequestOutcome::InProgress { started_at } => (
            DashboardStatsRefreshStatus::InProgress,
            None,
            Some(started_at.to_rfc3339()),
            None,
        ),
        RefreshRequestOutcome::Cooldown { retry_after } => (
            DashboardStatsRefreshStatus::Queued,
            None,
            None,
            Some(RefreshRequestOutcome::cooldown_error(retry_after)),
        ),
    };
    let audit_status = match (&error, status) {
        (Some(_), _) => "cooldown",
        (None, DashboardStatsRefreshStatus::Queued) => "queued",
        (None, DashboardStatsRefreshStatus::InProgress) => "in_progress",
    };
    state.auditor.record(AuditEvent {
        operator_identity: operator.operator_id.clone(),
        action_kind: kinds::STATS_REFRESH,
        target_account_id: None,
        payload: json!({
            "status": audit_status,
            "current_as_of": current_as_of,
        }),
        outcome: if error.is_some() {
            AuditOutcome::Denied
        } else {
            AuditOutcome::Success
        },
        error_code: error.as_ref().map(|e| e.code().to_string()),
        client_ip,
    });
    if let Some(error) = error {
        return Err(error);
    }
    Ok(DashboardStatsRefreshResponse {
        status,
        requested_at,
        started_at,
        current_as_of,
        cooldown_seconds: STATS_REFRESH_COOLDOWN.as_secs(),
    })
}

/// Build the stats response from the latest published snapshot.
///
/// Errors:
///   - [`GuardianError::DataUnavailable`] (`503`, retryable) until the
///     background refresher has published its first snapshot.
pub fn get_dashboard_stats(
    state: &AppState,
    updated_since: Option<DateTime<Utc>>,
) -> Result<DashboardStatsResponse> {
    let snapshot = state.dashboard.stats().current().ok_or_else(|| {
        GuardianError::DataUnavailable(
            "dashboard stats aggregate has not been published yet; retry after the first refresh"
                .to_string(),
        )
    })?;
    Ok(render(
        &snapshot,
        updated_since,
        state.dashboard.stats_refresh_interval().as_secs(),
    ))
}

fn render(
    snapshot: &StatsSnapshot,
    updated_since: Option<DateTime<Utc>>,
    refresh_interval_seconds: u64,
) -> DashboardStatsResponse {
    let counts = &snapshot.accounts;
    let accounts = DashboardAccountStats {
        total: counts.total,
        by_lifecycle: DashboardLifecycleCounts {
            active: counts.active,
            paused: counts.paused,
            released: counts.released,
        },
        by_auth_method: counts.by_auth_method.clone(),
        by_auth_method_and_signer_count: counts
            .by_auth_method_and_signer_count
            .iter()
            .map(
                |((auth_method, signer_count), count)| DashboardAuthMethodSignerCount {
                    auth_method: auth_method.clone(),
                    authorized_signer_count: *signer_count as u64,
                    count: *count,
                },
            )
            .collect(),
        updated_within_7d: counts.updated_within_7d,
        updated_within_30d: counts.updated_within_30d,
    };
    DashboardStatsResponse {
        as_of: snapshot.as_of.to_rfc3339(),
        updated_since: updated_since.map(|ts| ts.to_rfc3339()),
        refresh_interval_seconds,
        version: snapshot.version,
        accounts,
        assets: render_assets(&snapshot.assets_since(updated_since)),
    }
}

fn render_assets(aggregate: &AssetAggregate) -> DashboardAssetStats {
    DashboardAssetStats {
        eligible: aggregate.eligible,
        covered: aggregate.covered,
        skipped: aggregate.skipped.clone(),
        complete: aggregate.complete(),
        fungible: aggregate
            .fungible
            .iter()
            .map(|(faucet_id, total)| DashboardFungibleTotal {
                faucet_id: faucet_id.clone(),
                total_amount: total.to_string(),
            })
            .collect(),
        non_fungible: aggregate
            .non_fungible
            .iter()
            .map(|(faucet_id, count)| DashboardNonFungibleTotal {
                faucet_id: faucet_id.clone(),
                count: *count,
            })
            .collect(),
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::dashboard::stats::{
        AccountLifecycle, AccountStatsRecord, InventoryAggregates, SkipReason, StatsSnapshot,
        VaultOutcome, VaultSummary,
    };
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};
    use std::sync::Arc;

    async fn test_state() -> AppState {
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        AppState {
            storage: Arc::new(MockStorageBackend::new()),
            metadata: Arc::new(MockMetadataStore::new()),
            network_client: Arc::new(MockNetworkClient::new()),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::fixed("2026-09-11T12:00:00Z")),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    fn ts(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn record(
        id: &str,
        updated_at: &str,
        auth_method: &str,
        signers: usize,
        lifecycle: AccountLifecycle,
        vault: VaultOutcome,
    ) -> AccountStatsRecord {
        AccountStatsRecord {
            account_id: id.to_string(),
            updated_at: Some(ts(updated_at)),
            auth_method: auth_method.to_string(),
            authorized_signer_count: signers,
            lifecycle,
            state_commitment: None,
            vault,
        }
    }

    fn decoded(fungible: &[(&str, u64)], non_fungible: &[(&str, u64)]) -> VaultOutcome {
        VaultOutcome::Decoded {
            vault: VaultSummary {
                fungible: fungible.iter().map(|(f, a)| (f.to_string(), *a)).collect(),
                non_fungible: non_fungible
                    .iter()
                    .map(|(f, c)| (f.to_string(), *c))
                    .collect(),
            },
        }
    }

    fn snapshot(as_of: DateTime<Utc>, records: Vec<AccountStatsRecord>) -> StatsSnapshot {
        StatsSnapshot::new(3, as_of, as_of, records, InventoryAggregates::default())
    }

    #[test]
    fn parse_updated_since_accepts_rfc3339_and_normalizes_to_utc() {
        let parsed = parse_updated_since(Some("2026-09-04T02:00:00+02:00"))
            .unwrap()
            .unwrap();
        assert_eq!(parsed, ts("2026-09-04T00:00:00Z"));
        assert_eq!(parse_updated_since(None).unwrap(), None);
        // Blank is treated as absent, like the global-feed status filter.
        assert_eq!(parse_updated_since(Some("")).unwrap(), None);
        assert_eq!(parse_updated_since(Some("   ")).unwrap(), None);
    }

    #[test]
    fn parse_updated_since_rejects_non_rfc3339_with_stable_code() {
        for raw in [
            "yesterday",
            "2026-09-04",
            "1757548800",
            "2026-09-04T00:00:00",
        ] {
            let err = parse_updated_since(Some(raw)).unwrap_err();
            assert!(
                matches!(err, GuardianError::InvalidTimestamp(_)),
                "{raw:?} should be rejected, got {err:?}"
            );
            assert_eq!(err.code(), "invalid_timestamp");
        }
    }

    #[tokio::test]
    async fn returns_data_unavailable_until_first_refresh() {
        let state = test_state().await;
        let err = get_dashboard_stats(&state, None).unwrap_err();
        assert!(matches!(err, GuardianError::DataUnavailable(_)));
        assert!(err.retryable());
    }

    #[tokio::test]
    async fn renders_snapshot_into_wire_shape_with_coverage_and_decimal_strings() {
        let state = test_state().await;
        let as_of = ts("2026-09-11T00:00:00Z");
        let huge = u64::MAX;
        let records = vec![
            record(
                "a",
                "2026-09-10T00:00:00Z",
                "miden_falcon",
                2,
                AccountLifecycle::Active,
                decoded(&[("0xf1", huge)], &[("0xn1", 1)]),
            ),
            record(
                "b",
                "2026-09-10T00:00:00Z",
                "miden_falcon",
                1,
                AccountLifecycle::Paused,
                VaultOutcome::Skipped {
                    reason: SkipReason::StateUnavailable,
                },
            ),
            record(
                "c",
                "2026-01-01T00:00:00Z",
                "miden_ecdsa",
                1,
                AccountLifecycle::Released,
                decoded(&[("0xf1", 5), ("0xf2", 9)], &[]),
            ),
            record(
                "d",
                "2026-09-10T00:00:00Z",
                "evm",
                3,
                AccountLifecycle::Active,
                VaultOutcome::NotApplicable,
            ),
        ];
        state
            .dashboard
            .stats()
            .publish(Arc::new(snapshot(as_of, records)));

        let since = ts("2026-09-01T00:00:00Z");
        let stats = get_dashboard_stats(&state, Some(since)).unwrap();
        assert_eq!(stats.as_of, as_of.to_rfc3339());
        assert_eq!(
            stats.updated_since.as_deref(),
            Some("2026-09-01T00:00:00+00:00")
        );
        assert_eq!(stats.refresh_interval_seconds, 300);
        assert_eq!(stats.version, 3);

        // Account counts are unfiltered.
        let accounts = &stats.accounts;
        assert_eq!(accounts.total, 4);
        assert_eq!(
            accounts.by_lifecycle,
            DashboardLifecycleCounts {
                active: 2,
                paused: 1,
                released: 1
            }
        );
        assert_eq!(accounts.by_auth_method["miden_falcon"], 2);
        assert_eq!(accounts.by_auth_method["evm"], 1);
        assert_eq!(
            accounts.by_auth_method_and_signer_count,
            vec![
                DashboardAuthMethodSignerCount {
                    auth_method: "evm".into(),
                    authorized_signer_count: 3,
                    count: 1
                },
                DashboardAuthMethodSignerCount {
                    auth_method: "miden_ecdsa".into(),
                    authorized_signer_count: 1,
                    count: 1
                },
                DashboardAuthMethodSignerCount {
                    auth_method: "miden_falcon".into(),
                    authorized_signer_count: 1,
                    count: 1
                },
                DashboardAuthMethodSignerCount {
                    auth_method: "miden_falcon".into(),
                    authorized_signer_count: 2,
                    count: 1
                },
            ]
        );
        assert_eq!(accounts.updated_within_7d, 3);
        assert_eq!(accounts.updated_within_30d, 3);

        // Assets are filtered: `c` is too old, `d` is EVM, `b` is skipped.
        let assets = &stats.assets;
        assert_eq!((assets.eligible, assets.covered), (2, 1));
        assert_eq!(assets.skipped.get(&SkipReason::StateUnavailable), Some(&1));
        assert!(!assets.complete);
        assert_eq!(
            assets.fungible,
            vec![DashboardFungibleTotal {
                faucet_id: "0xf1".into(),
                total_amount: "18446744073709551615".into()
            }]
        );
        assert_eq!(
            assets.non_fungible,
            vec![DashboardNonFungibleTotal {
                faucet_id: "0xn1".into(),
                count: 1
            }]
        );

        // Unfiltered: everything Miden is eligible, `c` adds its faucets.
        let all = get_dashboard_stats(&state, None).unwrap();
        assert_eq!(all.updated_since, None);
        assert_eq!((all.assets.eligible, all.assets.covered), (3, 2));
        assert_eq!(all.assets.fungible.len(), 2);
        assert_eq!(all.assets.fungible[0].total_amount, "18446744073709551620");
        assert_eq!(
            all.assets.fungible[1],
            DashboardFungibleTotal {
                faucet_id: "0xf2".into(),
                total_amount: "9".into()
            }
        );

        // Wire JSON: amounts are strings, coverage is explicit.
        let json = serde_json::to_value(&stats).unwrap();
        assert!(json["assets"]["fungible"][0]["total_amount"].is_string());
        assert_eq!(json["assets"]["complete"], false);
        assert_eq!(json["assets"]["skipped"]["state_unavailable"], 1);
        assert!(json.get("degraded_aggregates").is_none());
    }
}
