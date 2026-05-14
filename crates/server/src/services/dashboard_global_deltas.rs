//! Global cross-account delta feed dashboard endpoint service.
//!
//! Spec reference: `005-operator-dashboard-metrics` FR-031..FR-035, US6.
//!
//! Returns delta records aggregated across all configured accounts,
//! ordered by `status_timestamp DESC` with `(account_id, nonce)` as the
//! stable tie-breaker. Surfaces only the lifecycle statuses persisted
//! in `deltas` (`candidate`, `canonical`, `discarded`); pending entries
//! live in `delta_proposals` and are exposed only through the global
//! proposal feed.
//!
//! ## Cursor stability
//!
//! Per FR-005: cursor traversal is stable under concurrent inserts but
//! an entry whose `status_timestamp` is bumped mid-traversal (e.g. a
//! candidate transitioning to canonical) MAY be skipped or repeated.
//! The composite tie-breaker on `(account_id, nonce)` guarantees a
//! deterministic order within each timestamp bucket.
//!
//! ## Filesystem-backend degradation (FR-029)
//!
//! On the **filesystem backend**, above the configured
//! `filesystem_aggregate_threshold` (default 1,000 accounts) this
//! endpoint short-circuits to `GuardianError::DataUnavailable` rather
//! than fan out across every account directory. The **Postgres
//! backend** serves this feed from indexed columns and is NOT bounded
//! by the threshold — `enforce_aggregate_threshold` returns early
//! when `storage.kind() != Filesystem`. Operators on Postgres should
//! not expect a threshold-induced 503 from this endpoint.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::dashboard::cursor::{self, Cursor, CursorKind};
use crate::delta_object::DeltaObject;
use crate::error::{GuardianError, Result};
use crate::services::dashboard_account_deltas::{DashboardDeltaStatus, decode_delta_status};
use crate::services::dashboard_pagination::{PagedResult, enforce_aggregate_threshold};
use crate::state::AppState;
use crate::storage::{DeltaStatusKind, GlobalDeltaCursor};

/// One entry in the global delta feed wire shape per `data-model.md`.
/// Carries every field of a per-account [`DashboardDeltaEntry`] plus
/// `account_id` so the dashboard can group / link without a second
/// request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardGlobalDeltaEntry {
    pub account_id: String,
    pub nonce: u64,
    pub status: DashboardDeltaStatus,
    pub status_timestamp: String,
    pub prev_commitment: String,
    pub new_commitment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
    /// See `DashboardDeltaEntry::proposal_type` — `None` for single-key
    /// Miden `push_delta` writes and EVM deltas.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_type: Option<String>,
}

/// Parse a comma-separated `?status=` filter into a typed allow-list
/// of lifecycle statuses. Unknown or empty entries surface as
/// [`GuardianError::InvalidStatusFilter`] per FR-033.
pub fn parse_status_filter(raw: Option<&str>) -> Result<Option<Vec<DashboardDeltaStatus>>> {
    let Some(s) = raw else {
        return Ok(None);
    };
    if s.is_empty() {
        // `?status=` with no value behaves like the parameter being
        // omitted: include every surfaced lifecycle status.
        return Ok(None);
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut out = Vec::new();
    for token in s.split(',') {
        let t = token.trim();
        if t.is_empty() {
            return Err(GuardianError::InvalidStatusFilter(format!(
                "empty status token in filter '{s}'"
            )));
        }
        if !seen.insert(t) {
            // Duplicate values are silently coalesced — caller intent
            // is unambiguous and tolerating duplicates is friendlier
            // than rejecting.
            continue;
        }
        let parsed = match t {
            "candidate" => DashboardDeltaStatus::Candidate,
            "canonical" => DashboardDeltaStatus::Canonical,
            "discarded" => DashboardDeltaStatus::Discarded,
            other => {
                return Err(GuardianError::InvalidStatusFilter(format!(
                    "unknown status value '{other}'; allowed: candidate, canonical, discarded"
                )));
            }
        };
        out.push(parsed);
    }
    Ok(Some(out))
}

fn entry_from(delta: &DeltaObject, account_id: &str) -> Option<DashboardGlobalDeltaEntry> {
    let (status, retry_count, status_timestamp) = decode_delta_status(&delta.status)?;
    Some(DashboardGlobalDeltaEntry {
        account_id: account_id.to_string(),
        nonce: delta.nonce,
        status,
        status_timestamp,
        prev_commitment: delta.prev_commitment.clone(),
        new_commitment: delta.new_commitment.clone(),
        retry_count,
        proposal_type: delta.proposal_type().map(str::to_string),
    })
}

fn map_status_filter(status: &DashboardDeltaStatus) -> DeltaStatusKind {
    match status {
        DashboardDeltaStatus::Candidate => DeltaStatusKind::Candidate,
        DashboardDeltaStatus::Canonical => DeltaStatusKind::Canonical,
        DashboardDeltaStatus::Discarded => DeltaStatusKind::Discarded,
    }
}

fn build_storage_cursor(c: &Cursor) -> Option<GlobalDeltaCursor> {
    match (&c.last_updated_at, &c.last_account_id, c.last_nonce) {
        (Some(ts), Some(account_id), Some(last_nonce)) => Some(GlobalDeltaCursor {
            last_status_timestamp: *ts,
            last_account_id: account_id.clone(),
            last_nonce,
        }),
        _ => None,
    }
}

/// List delta records across all configured accounts, paginated
/// newest-first by `status_timestamp DESC`.
///
/// Errors:
///   - [`GuardianError::DataUnavailable`] when above the configured
///     filesystem aggregate threshold (FR-029).
///   - [`GuardianError::InvalidCursor`] if the supplied cursor is for
///     the wrong endpoint kind.
pub async fn list_global_deltas(
    state: &AppState,
    limit: u32,
    cursor: Option<Cursor>,
    status_filter: Option<Vec<DashboardDeltaStatus>>,
) -> Result<PagedResult<DashboardGlobalDeltaEntry>> {
    if let Some(c) = cursor.as_ref()
        && c.kind != CursorKind::GlobalDeltas
    {
        return Err(GuardianError::InvalidCursor(
            "expected GlobalDeltas cursor kind".to_string(),
        ));
    }

    enforce_aggregate_threshold(state, "global delta feed").await?;

    let storage_filter: Option<Vec<DeltaStatusKind>> = status_filter
        .as_ref()
        .map(|allow| allow.iter().map(map_status_filter).collect());

    let storage_cursor = cursor.as_ref().and_then(build_storage_cursor);
    let page_size = limit.saturating_add(1);
    let rows = state
        .storage
        .list_global_deltas_paged(page_size, storage_cursor, storage_filter)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "global delta feed: storage read failed");
            GuardianError::DataUnavailable(format!("Failed to load global delta feed: {e}"))
        })?;

    // Derive `has_more` from the *raw* storage rows so that if any
    // row gets dropped by `entry_from` (e.g. an unexpected `Pending`
    // surfacing on the deltas table), we still emit a cursor when
    // more rows exist. Deriving from `entries.len()` after
    // `filter_map` would silently truncate pagination.
    let limit_us = limit as usize;
    let has_more = rows.len() > limit_us;

    let mut entries: Vec<DashboardGlobalDeltaEntry> = rows
        .iter()
        .filter_map(|row| entry_from(&row.delta, &row.account_id))
        .collect();
    entries.truncate(limit_us);

    let next_cursor = if has_more {
        match entries.last() {
            Some(last) => {
                let ts = DateTime::parse_from_rfc3339(&last.status_timestamp)
                    .map(|dt| dt.with_timezone(&Utc))
                    .map_err(|e| {
                        GuardianError::StorageError(format!(
                            "global delta cursor: stored status_timestamp is not RFC3339 for ('{}', nonce {}): '{}': {e}",
                            last.account_id, last.nonce, last.status_timestamp
                        ))
                    })?;
                let cursor = Cursor::global_deltas(ts, last.account_id.clone(), last.nonce as i64);
                Some(cursor::encode(&cursor, state.dashboard.cursor_secret())?)
            }
            None => None,
        }
    } else {
        None
    };

    Ok(PagedResult::new(entries, next_cursor))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::testing::mocks::{MockMetadataStore, MockStorageBackend};
    use std::sync::Arc;

    async fn build_state(
        account_ids: Vec<String>,
        deltas_per_account: Vec<Vec<DeltaObject>>,
    ) -> AppState {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let metadata = MockMetadataStore::new().with_list(Ok(account_ids));
        // The mock uses LIFO `.pop()`, so push in reverse order so
        // that `deltas_per_account[i]` corresponds to
        // `account_ids[i]` from the caller's perspective.
        let mut storage = MockStorageBackend::new();
        for d in deltas_per_account.into_iter().rev() {
            storage = storage.with_pull_deltas_after(Ok(d));
        }
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");

        AppState {
            storage: Arc::new(storage),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    // --- parse_status_filter ---

    #[test]
    fn parse_status_filter_omitted_returns_none() {
        assert_eq!(parse_status_filter(None).unwrap(), None);
    }

    #[test]
    fn parse_status_filter_empty_value_returns_none() {
        // `?status=` (empty) behaves like the parameter being omitted.
        assert_eq!(parse_status_filter(Some("")).unwrap(), None);
    }

    #[test]
    fn parse_status_filter_single_value_accepted() {
        assert_eq!(
            parse_status_filter(Some("candidate")).unwrap(),
            Some(vec![DashboardDeltaStatus::Candidate])
        );
    }

    #[test]
    fn parse_status_filter_csv_accepted() {
        assert_eq!(
            parse_status_filter(Some("candidate,canonical")).unwrap(),
            Some(vec![
                DashboardDeltaStatus::Candidate,
                DashboardDeltaStatus::Canonical,
            ])
        );
    }

    #[test]
    fn parse_status_filter_dedups_silently() {
        assert_eq!(
            parse_status_filter(Some("candidate,candidate")).unwrap(),
            Some(vec![DashboardDeltaStatus::Candidate])
        );
    }

    #[test]
    fn parse_status_filter_rejects_unknown_value() {
        let err = parse_status_filter(Some("foo")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidStatusFilter(_)));
        assert_eq!(err.code(), "invalid_status_filter");
    }

    #[test]
    fn parse_status_filter_rejects_pending_value() {
        // `pending` is a valid lifecycle status but lives in
        // delta_proposals, not deltas. The global delta feed must
        // reject it so consumers don't expect it.
        let err = parse_status_filter(Some("pending")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidStatusFilter(_)));
    }

    #[test]
    fn parse_status_filter_rejects_empty_token_in_csv() {
        let err = parse_status_filter(Some("candidate,,canonical")).unwrap_err();
        assert!(matches!(err, GuardianError::InvalidStatusFilter(_)));
    }

    // --- list_global_deltas ---
    //
    // Sort/filter/pagination behavior moved to the storage layer in
    // feature `005-operator-dashboard-metrics` Decision 1 (revised).
    // Coverage for those concerns lives at the storage layer and the
    // integration tests in `crates/server/src/api/dashboard_feeds.rs`.
    // The service-layer tests below exercise what the service still
    // owns: cursor-kind validation and (for backward-compat with the
    // pre-Decision-1 fixtures that exercised the threshold)
    // filesystem-threshold short-circuit when above-threshold inventories
    // bypass the storage call entirely.

    #[tokio::test]
    async fn rejects_cursor_with_wrong_kind() {
        let state = build_state(Vec::new(), Vec::new()).await;
        let wrong = Cursor::account_deltas(5);
        let err = list_global_deltas(&state, 50, Some(wrong), None)
            .await
            .unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));
    }

    #[tokio::test]
    async fn storage_failure_surfaces_as_data_unavailable() {
        // Storage returns Err; service must map to DataUnavailable.
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;
        let metadata = MockMetadataStore::new();
        let storage = MockStorageBackend::new()
            .with_list_global_deltas_paged(Err("storage unreachable".into()));
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
        let err = list_global_deltas(&state, 50, None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, GuardianError::DataUnavailable(_)));
        assert_eq!(err.code(), "data_unavailable");
    }
}
