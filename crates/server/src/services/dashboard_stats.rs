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

use crate::dashboard::stats::{AssetAggregate, StatsSnapshot};
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
    /// Counts by stable auth-method label. Never omitted above an
    /// inventory threshold: the aggregate is maintained incrementally.
    pub by_auth_method: BTreeMap<String, u64>,
    /// Sorted by `(auth_method, authorized_signer_count)`.
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
    /// Eligible accounts not covered, by stable reason
    /// (`state_unavailable`, `state_undecodable`).
    pub skipped: BTreeMap<String, u64>,
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
    /// RFC3339 time the published aggregate was computed. Consumers
    /// derive the result's age from this; it advances by at most
    /// `refresh_interval_seconds` at steady state.
    pub as_of: String,
    /// The applied filter, normalized to RFC3339, or `null` when the
    /// asset aggregate spans every account.
    pub updated_since: Option<String>,
    /// Configured cadence of the background refresh
    /// (`GUARDIAN_DASHBOARD_STATS_REFRESH_INTERVAL_SECS`).
    pub refresh_interval_seconds: u64,
    pub accounts: DashboardAccountStats,
    pub assets: DashboardAssetStats,
    /// Stable names of aggregates the server declined to compute for
    /// this response. Reserved: the current implementation publishes
    /// `accounts` and `assets` atomically, so this is always empty and
    /// unavailability is reported as `503 data_unavailable` instead.
    pub degraded_aggregates: Vec<String>,
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
            "dashboard stats aggregate has not been computed yet; retry after the first refresh"
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
        accounts,
        assets: render_assets(&snapshot.assets_since(updated_since)),
        degraded_aggregates: Vec::new(),
    }
}

fn render_assets(aggregate: &AssetAggregate) -> DashboardAssetStats {
    DashboardAssetStats {
        eligible: aggregate.eligible,
        covered: aggregate.covered,
        skipped: aggregate
            .skipped
            .iter()
            .map(|(reason, count)| (reason.as_str().to_string(), *count))
            .collect(),
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
        AccountLifecycle, AccountStatsRecord, SkipReason, StatsSnapshot, VaultOutcome, VaultSummary,
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
        auth_method: &'static str,
        signers: usize,
        lifecycle: AccountLifecycle,
        vault: VaultOutcome,
    ) -> AccountStatsRecord {
        AccountStatsRecord {
            account_id: id.to_string(),
            updated_at: Some(ts(updated_at)),
            auth_method,
            authorized_signer_count: signers,
            lifecycle,
            state_commitment: None,
            vault,
        }
    }

    fn decoded(fungible: &[(&str, u128)], non_fungible: &[(&str, u64)]) -> VaultOutcome {
        VaultOutcome::Decoded(VaultSummary {
            fungible: fungible.iter().map(|(f, a)| (f.to_string(), *a)).collect(),
            non_fungible: non_fungible
                .iter()
                .map(|(f, c)| (f.to_string(), *c))
                .collect(),
        })
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
        let huge = u128::from(u64::MAX) + 1;
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
                VaultOutcome::Skipped(SkipReason::StateUnavailable),
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
            .publish(Arc::new(StatsSnapshot::new(as_of, records)));

        let since = ts("2026-09-01T00:00:00Z");
        let stats = get_dashboard_stats(&state, Some(since)).unwrap();
        assert_eq!(stats.as_of, as_of.to_rfc3339());
        assert_eq!(
            stats.updated_since.as_deref(),
            Some("2026-09-01T00:00:00+00:00")
        );
        assert_eq!(stats.refresh_interval_seconds, 60);
        assert!(stats.degraded_aggregates.is_empty());

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
        assert_eq!(assets.skipped.get("state_unavailable"), Some(&1));
        assert!(!assets.complete);
        assert_eq!(
            assets.fungible,
            vec![DashboardFungibleTotal {
                faucet_id: "0xf1".into(),
                total_amount: "18446744073709551616".into()
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
        assert_eq!(all.assets.fungible[0].total_amount, "18446744073709551621");
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
        assert_eq!(json["degraded_aggregates"], serde_json::json!([]));
    }
}
