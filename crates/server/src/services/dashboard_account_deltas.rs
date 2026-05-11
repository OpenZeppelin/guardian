//! Per-account delta history dashboard endpoint service.
//!
//! Spec reference: `005-operator-dashboard-metrics` FR-013..FR-016, US3.
//!
//! Returns the persisted delta history for one account with newest-first
//! ordering by `nonce DESC`. Surfaces only the lifecycle statuses that
//! live in the `deltas` table (`candidate`, `canonical`, `discarded`).
//! `pending` entries live in `delta_proposals` and are exposed via
//! [`crate::services::dashboard_account_proposals`] per FR-014.
//!
//! Cursor traversal is fully stable: `nonce` is per-account immutable
//! and monotonic, so concurrent status updates do not move an entry's
//! position in the ordering (research.md Decision 1).

use serde::Serialize;

use crate::dashboard::cursor::{self, Cursor, CursorKind};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::services::dashboard_pagination::PagedResult;
use crate::state::AppState;
use crate::storage::AccountDeltaCursor;

/// Lifecycle status surfaced on the per-account delta history endpoint.
/// `pending`-status records live in `delta_proposals` and are
/// surfaced via the proposal queue endpoint instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DashboardDeltaStatus {
    Candidate,
    Canonical,
    Discarded,
}

/// One entry in the delta history wire shape per `data-model.md`.
/// `account_id` is omitted on per-account responses (the path scopes
/// it). The global delta feed (Phase 8) wraps this struct with
/// `account_id` so a single shape is shared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardDeltaEntry {
    pub nonce: u64,
    pub status: DashboardDeltaStatus,
    pub status_timestamp: String,
    pub prev_commitment: String,
    /// `None` is serialized as `null` rather than skipped, since the
    /// spec exposes `new_commitment: string | null` (e.g. for a
    /// discarded delta that did not produce a resulting commitment).
    pub new_commitment: Option<String>,
    /// Always `Some(_)` on candidate entries (default `0` per FR-015);
    /// `None` and skipped on `canonical` / `discarded`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_count: Option<u32>,
}

/// Decode a [`DeltaStatus`] into the dashboard wire triple
/// `(status, retry_count, status_timestamp)`. Returns `None` for
/// `Pending` (those live on the proposal feed). Shared with the global
/// delta feed.
pub(crate) fn decode_delta_status(
    status: &DeltaStatus,
) -> Option<(DashboardDeltaStatus, Option<u32>, String)> {
    match status {
        DeltaStatus::Pending { .. } => None,
        DeltaStatus::Candidate {
            timestamp,
            retry_count,
        } => Some((
            DashboardDeltaStatus::Candidate,
            Some(*retry_count),
            timestamp.clone(),
        )),
        DeltaStatus::Canonical { timestamp } => {
            Some((DashboardDeltaStatus::Canonical, None, timestamp.clone()))
        }
        DeltaStatus::Discarded { timestamp } => {
            Some((DashboardDeltaStatus::Discarded, None, timestamp.clone()))
        }
    }
}

impl DashboardDeltaEntry {
    /// Build a wire entry from a persisted [`DeltaObject`]. Returns
    /// `None` for `Pending` deltas, which the caller filters out.
    fn from_delta(delta: &DeltaObject) -> Option<Self> {
        let (status, retry_count, status_timestamp) = decode_delta_status(&delta.status)?;
        Some(Self {
            nonce: delta.nonce,
            status,
            status_timestamp,
            prev_commitment: delta.prev_commitment.clone(),
            new_commitment: delta.new_commitment.clone(),
            retry_count,
        })
    }
}

/// List the persisted delta history for `account_id`, paginated
/// newest-first by `nonce DESC`.
///
/// Errors:
///   - [`GuardianError::AccountNotFound`] when no metadata exists for
///     `account_id`.
///   - [`GuardianError::DataUnavailable`] when metadata exists but the
///     delta records cannot be loaded (FR-022).
///   - [`GuardianError::InvalidCursor`] is propagated from the caller's
///     cursor parsing; this function never produces it.
pub async fn list_account_deltas(
    state: &AppState,
    account_id: &str,
    limit: u32,
    cursor: Option<Cursor>,
) -> Result<PagedResult<DashboardDeltaEntry>> {
    // Reject any cursor whose kind doesn't match — defensive; the caller
    // is expected to have already type-checked via parse_cursor.
    if let Some(c) = cursor.as_ref()
        && c.kind != CursorKind::AccountDeltas
    {
        return Err(GuardianError::InvalidCursor(
            "expected AccountDeltas cursor kind".to_string(),
        ));
    }

    // 404 vs 503 disambiguation per FR-022: metadata-missing → 404,
    // metadata-present-but-storage-fails → 503.
    let metadata_exists = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| {
            GuardianError::StorageError(format!("Failed to load metadata for '{account_id}': {e}"))
        })?
        .is_some();
    if !metadata_exists {
        return Err(GuardianError::AccountNotFound(account_id.to_string()));
    }

    // Fetch one page-plus-one from the storage layer so we can detect
    // end-of-list and emit a `next_cursor` only when more rows exist.
    let storage_cursor = cursor.as_ref().and_then(|c| {
        c.last_nonce
            .map(|last_nonce| AccountDeltaCursor { last_nonce })
    });
    let page_size = limit.saturating_add(1);
    let rows = state
        .storage
        .list_account_deltas_paged(account_id, page_size, storage_cursor)
        .await
        .map_err(|e| {
            tracing::warn!(
                account_id = %account_id,
                error = %e,
                "dashboard delta history could not load deltas"
            );
            GuardianError::DataUnavailable(format!(
                "Failed to load delta history for '{account_id}': {e}"
            ))
        })?;

    let mut entries: Vec<DashboardDeltaEntry> = rows
        .iter()
        .filter_map(DashboardDeltaEntry::from_delta)
        .collect();

    let limit_us = limit as usize;
    let has_more = entries.len() > limit_us;
    entries.truncate(limit_us);

    let next_cursor = if has_more {
        entries.last().map(|last| {
            let next = Cursor::account_deltas(last.nonce as i64);
            cursor::encode(&next, state.dashboard.cursor_secret())
        })
    } else {
        None
    }
    .transpose()?;

    Ok(PagedResult::new(entries, next_cursor))
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::testing::mocks::{MockMetadataStore, MockStorageBackend};
    use std::sync::Arc;

    fn delta(nonce: u64, status: DeltaStatus) -> DeltaObject {
        DeltaObject {
            account_id: "0xacc".to_string(),
            nonce,
            prev_commitment: format!("0xprev{nonce}"),
            new_commitment: Some(format!("0xnew{nonce}")),
            delta_payload: serde_json::json!({}),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status,
        }
    }

    fn candidate(nonce: u64, retries: u32) -> DeltaObject {
        delta(
            nonce,
            DeltaStatus::Candidate {
                timestamp: format!("2026-05-08T12:0{nonce}:00Z"),
                retry_count: retries,
            },
        )
    }

    fn canonical(nonce: u64) -> DeltaObject {
        delta(
            nonce,
            DeltaStatus::Canonical {
                timestamp: format!("2026-05-08T12:0{nonce}:00Z"),
            },
        )
    }

    #[allow(dead_code)] // referenced in upcoming Phase 5 acceptance test additions
    fn discarded(nonce: u64) -> DeltaObject {
        delta(
            nonce,
            DeltaStatus::Discarded {
                timestamp: format!("2026-05-08T12:0{nonce}:00Z"),
            },
        )
    }

    async fn state_with_n_calls(
        deltas: Vec<DeltaObject>,
        has_metadata: bool,
        repeat: usize,
    ) -> AppState {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::metadata::AccountMetadata;
        use crate::metadata::auth::Auth;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let metadata_response = if has_metadata {
            Ok(Some(AccountMetadata {
                account_id: "0xacc".to_string(),
                auth: Auth::MidenFalconRpo {
                    cosigner_commitments: vec!["0xc1".into()],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                created_at: "2026-05-01T00:00:00Z".into(),
                updated_at: "2026-05-01T00:00:00Z".into(),
                has_pending_candidate: false,
                last_auth_timestamp: None,
            }))
        } else {
            Ok(None)
        };

        let mut metadata_store = MockMetadataStore::new();
        for _ in 0..repeat {
            metadata_store = metadata_store.with_get(metadata_response.clone());
        }

        let mut storage = MockStorageBackend::new();
        for _ in 0..repeat {
            storage = storage.with_list_account_deltas_paged(Ok(deltas.clone()));
        }

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
    async fn returns_404_for_unknown_account() {
        let state = state_with_n_calls(Vec::new(), false, 1).await;
        let err = list_account_deltas(&state, "0xacc", 50, None)
            .await
            .unwrap_err();
        assert!(matches!(err, GuardianError::AccountNotFound(_)));
    }

    #[tokio::test]
    async fn returns_empty_page_for_known_account_with_no_deltas() {
        let state = state_with_n_calls(Vec::new(), true, 1).await;
        let result = list_account_deltas(&state, "0xacc", 50, None)
            .await
            .unwrap();
        assert!(result.items.is_empty());
        assert!(result.next_cursor.is_none());
    }

    // Sort/filter behavior moved to the storage layer in feature
    // `005-operator-dashboard-metrics` Decision 1 (revised). Those
    // are exercised by the storage-layer impls and the integration
    // tests in `crates/server/src/api/dashboard_history.rs`. The
    // service-layer tests below focus on what the service still
    // owns: error mapping, cursor-kind validation, and wire-shape
    // serialization.

    #[tokio::test]
    async fn candidate_entries_carry_retry_count() {
        let state = state_with_n_calls(vec![candidate(5, 3)], true, 1).await;
        let result = list_account_deltas(&state, "0xacc", 50, None)
            .await
            .unwrap();
        assert_eq!(result.items[0].status, DashboardDeltaStatus::Candidate);
        assert_eq!(result.items[0].retry_count, Some(3));
    }

    #[tokio::test]
    async fn candidate_with_zero_retries_serializes_retry_count_zero() {
        let state = state_with_n_calls(vec![candidate(5, 0)], true, 1).await;
        let result = list_account_deltas(&state, "0xacc", 50, None)
            .await
            .unwrap();
        let json = serde_json::to_value(&result.items[0]).unwrap();
        assert_eq!(json["retry_count"], serde_json::json!(0));
    }

    #[tokio::test]
    async fn canonical_entry_omits_retry_count_in_serialized_form() {
        let state = state_with_n_calls(vec![canonical(5)], true, 1).await;
        let result = list_account_deltas(&state, "0xacc", 50, None)
            .await
            .unwrap();
        assert_eq!(result.items[0].retry_count, None);
        let json = serde_json::to_value(&result.items[0]).unwrap();
        assert!(
            json.get("retry_count").is_none(),
            "retry_count should be omitted on canonical entries: {json}"
        );
    }

    #[tokio::test]
    async fn rejects_cursor_with_wrong_kind() {
        let state = state_with_n_calls(Vec::new(), true, 1).await;
        let wrong = Cursor::account_proposals(5, "0xc".to_string());
        let err = list_account_deltas(&state, "0xacc", 5, Some(wrong))
            .await
            .unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));
    }

    /// FR-022 / SC-012: when metadata exists but the storage layer
    /// fails to load deltas for that account, the service must return
    /// `DataUnavailable` (HTTP 503), distinct from `AccountNotFound`
    /// (404).
    #[tokio::test]
    async fn returns_503_when_metadata_exists_but_storage_fails() {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let metadata =
            MockMetadataStore::new().with_get(Ok(Some(crate::metadata::AccountMetadata {
                account_id: "0xacc".into(),
                auth: crate::metadata::auth::Auth::MidenFalconRpo {
                    cosigner_commitments: vec!["0xc1".into()],
                },
                network_config: crate::metadata::NetworkConfig::miden_default(),
                created_at: "2026-05-01T00:00:00Z".into(),
                updated_at: "2026-05-01T00:00:00Z".into(),
                has_pending_candidate: false,
                last_auth_timestamp: None,
            })));
        let storage = MockStorageBackend::new()
            .with_list_account_deltas_paged(Err("disk read failed".into()));
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

        let err = list_account_deltas(&state, "0xacc", 50, None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, GuardianError::DataUnavailable(_)),
            "expected DataUnavailable, got {err:?}"
        );
        assert_eq!(err.code(), "data_unavailable");
        assert_eq!(
            err.http_status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// Bug #1 regression: walk multi-page cursor traversal end-to-end
    /// against the actual filesystem backend (the mock backend does
    /// not honor cursor arguments). Asserts no row is skipped or
    /// repeated. Pre-fix this terminated after one page on Postgres
    /// because the cursor encoded `nonce` but the storage layer
    /// filtered on the surrogate `id` column.
    #[tokio::test]
    async fn cursor_walks_every_page_no_skip_no_repeat() {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::storage::StorageBackend;
        use crate::storage::filesystem::FilesystemService;
        use crate::testing::mocks::MockNetworkClient;
        use tempfile::TempDir;
        use tokio::sync::Mutex;

        let dir = TempDir::new().expect("tempdir");
        let svc = FilesystemService::new(dir.path().to_path_buf())
            .await
            .expect("svc");

        // Seed 23 canonical deltas; page size 5 → 5 pages of 5 + 1 of 3.
        let total: u64 = 23;
        for i in 0..total {
            let delta = DeltaObject {
                account_id: "0xacc".into(),
                nonce: i,
                prev_commitment: format!("0xprev{i:04}"),
                new_commitment: Some(format!("0xnew{i:04}")),
                delta_payload: serde_json::json!({}),
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: DeltaStatus::Canonical {
                    timestamp: format!("2026-05-08T12:00:{:02}Z", i % 60),
                },
            };
            svc.submit_delta(&delta).await.expect("submit");
        }

        let metadata = {
            let mut m = MockMetadataStore::new();
            for _ in 0..10 {
                m = m.with_get(Ok(Some(crate::metadata::AccountMetadata {
                    account_id: "0xacc".into(),
                    auth: crate::metadata::auth::Auth::MidenFalconRpo {
                        cosigner_commitments: vec!["0xc1".into()],
                    },
                    network_config: crate::metadata::NetworkConfig::miden_default(),
                    created_at: "2026-05-01T00:00:00Z".into(),
                    updated_at: "2026-05-01T00:00:00Z".into(),
                    has_pending_candidate: false,
                    last_auth_timestamp: None,
                })));
            }
            m
        };

        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        let ack = AckRegistry::new(keystore_dir).await.expect("ack");
        let state = AppState {
            storage: Arc::new(svc),
            metadata: Arc::new(metadata),
            network_client: Arc::new(Mutex::new(MockNetworkClient::new())),
            ack,
            canonicalization: None,
            clock: Arc::new(MockClock::default()),
            dashboard: Arc::new(crate::dashboard::DashboardState::default()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };

        let limit = 5;
        let mut all_nonces: Vec<u64> = Vec::new();
        let mut next_cursor: Option<Cursor> = None;
        let mut pages = 0;
        for _ in 0..20 {
            let page = list_account_deltas(&state, "0xacc", limit, next_cursor)
                .await
                .expect("list");
            for entry in &page.items {
                all_nonces.push(entry.nonce);
            }
            pages += 1;
            match page.next_cursor {
                Some(encoded) => {
                    let decoded = cursor::decode(
                        &encoded,
                        state.dashboard.cursor_secret(),
                        CursorKind::AccountDeltas,
                    )
                    .expect("decode cursor");
                    next_cursor = Some(decoded);
                }
                None => break,
            }
        }
        assert_eq!(all_nonces.len(), total as usize, "every nonce returned");
        assert_eq!(pages, 5, "ceil(23/5)");
        // newest-first by nonce, no duplicates, full coverage
        let mut expected: Vec<u64> = (0..total).collect();
        expected.reverse();
        assert_eq!(all_nonces, expected);
    }
}
