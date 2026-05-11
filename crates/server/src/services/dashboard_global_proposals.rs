//! Global cross-account in-flight proposal feed dashboard endpoint
//! service.
//!
//! Spec reference: `005-operator-dashboard-metrics` FR-035..FR-037, US7.
//!
//! Returns multisig proposals across all accounts that are still
//! collecting cosigner signatures (`DeltaStatus::Pending` rows in
//! `delta_proposals`). The endpoint takes no `status` filter — every
//! entry is in-flight by definition (FR-035). EVM accounts
//! (`Auth::EvmEcdsa`) never appear in v1 (FR-017): EVM proposals are
//! tracked in a separate, feature-gated storage path that does not
//! flow through `delta_proposals`.
//!
//! ## Cursor stability
//!
//! Sort key is `(originating_timestamp DESC, account_id ASC,
//! commitment ASC)`. The originating timestamp is set when the
//! proposal enters the `Pending` state and is immutable while it
//! remains in the proposal queue (a transition to candidate /
//! canonical / discarded moves it out of the queue and out of this
//! feed). Cursor traversal is therefore fully stable for the lifetime
//! of a proposal in the queue.
//!
//! ## Filesystem-backend degradation (FR-029)
//!
//! Above the configured `filesystem_aggregate_threshold` (default
//! 1,000 accounts), this endpoint short-circuits to
//! [`GuardianError::DataUnavailable`] rather than fan out across
//! every account directory.

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::dashboard::cursor::{self, Cursor, CursorKind};
use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{GuardianError, Result};
use crate::metadata::auth::Auth;
use crate::services::dashboard_account_proposals::signatures_required;
use crate::services::dashboard_pagination::{PagedResult, enforce_aggregate_threshold};
use crate::state::AppState;
use crate::storage::GlobalProposalCursor;

/// One entry in the global proposal feed wire shape per
/// `data-model.md`. Includes every field of the per-account
/// proposal entry plus `account_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DashboardGlobalProposalEntry {
    pub account_id: String,
    pub commitment: String,
    pub nonce: u64,
    pub proposer_id: String,
    pub originating_timestamp: String,
    pub signatures_collected: u32,
    pub signatures_required: u32,
    pub prev_commitment: String,
    pub new_commitment: Option<String>,
}

fn entry_from(
    commitment: &str,
    proposal: &DeltaObject,
    account_id: &str,
    auth: &Auth,
) -> Option<DashboardGlobalProposalEntry> {
    let DeltaStatus::Pending {
        timestamp,
        proposer_id,
        cosigner_sigs,
    } = &proposal.status
    else {
        return None;
    };
    Some(DashboardGlobalProposalEntry {
        account_id: account_id.to_string(),
        commitment: commitment.to_string(),
        nonce: proposal.nonce,
        proposer_id: proposer_id.clone(),
        originating_timestamp: timestamp.clone(),
        signatures_collected: cosigner_sigs.len() as u32,
        signatures_required: signatures_required(auth),
        prev_commitment: proposal.prev_commitment.clone(),
        new_commitment: proposal.new_commitment.clone(),
    })
}

fn build_storage_cursor(c: &Cursor) -> Option<GlobalProposalCursor> {
    match (
        &c.last_updated_at,
        &c.last_account_id,
        c.last_nonce,
        &c.last_commitment,
    ) {
        (Some(ts), Some(account_id), Some(last_nonce), Some(last_commitment)) => {
            Some(GlobalProposalCursor {
                last_originating_timestamp: *ts,
                last_account_id: account_id.clone(),
                last_nonce,
                last_commitment: last_commitment.clone(),
            })
        }
        _ => None,
    }
}

/// List in-flight proposals across all configured accounts,
/// paginated newest-first by `originating_timestamp DESC`.
pub async fn list_global_proposals(
    state: &AppState,
    limit: u32,
    cursor: Option<Cursor>,
) -> Result<PagedResult<DashboardGlobalProposalEntry>> {
    if let Some(c) = cursor.as_ref()
        && c.kind != CursorKind::GlobalProposals
    {
        return Err(GuardianError::InvalidCursor(
            "expected GlobalProposals cursor kind".to_string(),
        ));
    }

    enforce_aggregate_threshold(state, "global proposal feed").await?;

    let storage_cursor = cursor.as_ref().and_then(build_storage_cursor);
    let page_size = limit.saturating_add(1);
    let mut records = state
        .storage
        .list_global_proposals_paged(page_size, storage_cursor)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "global proposal feed: storage read failed");
            GuardianError::DataUnavailable(format!("Failed to load global proposal feed: {e}"))
        })?;

    // `has_more` is judged from the raw storage count, BEFORE any
    // service-layer filtering. Otherwise EVM-account filtering (or any
    // other drop) would underflow `entries.len() > limit` and emit
    // `next_cursor = null` while more in-flight proposals exist. We
    // truncate to `limit` raw rows first, then build the cursor from
    // the last raw row that survived the truncation.
    let limit_us = limit as usize;
    let has_more = records.len() > limit_us;
    records.truncate(limit_us);
    let cursor_anchor = records.last().cloned();

    // Resolve each account's auth once so we can derive
    // `signatures_required` per FR-019 and drop EVM accounts per
    // FR-017. The cache treats EVM and vanished-metadata as `None` so
    // a duplicated account_id doesn't trigger repeat metadata reads.
    let mut entries: Vec<DashboardGlobalProposalEntry> = Vec::new();
    let mut auth_cache: std::collections::HashMap<String, Option<Auth>> =
        std::collections::HashMap::new();
    for record in &records {
        let auth_slot = match auth_cache.get(&record.account_id) {
            Some(slot) => slot.clone(),
            None => {
                let resolved = match state.metadata.get(&record.account_id).await.map_err(|e| {
                    GuardianError::StorageError(format!(
                        "Failed to load metadata for '{}': {}",
                        record.account_id, e
                    ))
                })? {
                    Some(metadata) if matches!(metadata.auth, Auth::EvmEcdsa { .. }) => None,
                    Some(metadata) => Some(metadata.auth),
                    None => None,
                };
                auth_cache.insert(record.account_id.clone(), resolved.clone());
                resolved
            }
        };
        let Some(auth) = auth_slot else {
            continue;
        };
        if let Some(entry) = entry_from(
            &record.commitment,
            &record.proposal,
            &record.account_id,
            &auth,
        ) {
            entries.push(entry);
        }
    }

    let next_cursor = if has_more {
        cursor_anchor.and_then(|anchor| {
            DateTime::parse_from_rfc3339(anchor.proposal.status.timestamp())
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
                .map(|ts| {
                    Cursor::global_proposals(
                        ts,
                        anchor.account_id.clone(),
                        anchor.proposal.nonce as i64,
                        anchor.commitment.clone(),
                    )
                })
                .map(|c| cursor::encode(&c, state.dashboard.cursor_secret()))
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
    use crate::metadata::AccountMetadata;
    use crate::testing::mocks::{MockMetadataStore, MockStorageBackend};
    use std::sync::Arc;

    fn account_metadata(account_id: &str, auth: Auth) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth,
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
        }
    }

    async fn build_state(
        account_metas: Vec<(String, Auth)>,
        proposals_per_account: Vec<Vec<DeltaObject>>,
    ) -> AppState {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;

        let account_ids: Vec<String> = account_metas.iter().map(|(id, _)| id.clone()).collect();

        // Mock metadata.get and storage.pull_* both use LIFO pop, so
        // push in reverse so the caller can write fixtures naturally.
        let mut metadata = MockMetadataStore::new().with_list(Ok(account_ids));
        for (id, auth) in account_metas.iter().rev() {
            metadata = metadata.with_get(Ok(Some(account_metadata(id, auth.clone()))));
        }

        let mut storage = MockStorageBackend::new();
        for proposals in proposals_per_account.into_iter().rev() {
            storage = storage.with_pull_all_delta_proposals(Ok(proposals));
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

    // Sort/filter/EVM-exclusion tests moved to the storage layer +
    // integration tests. Service-layer tests below cover what the
    // service still owns: cursor-kind validation and storage-error
    // mapping.

    #[tokio::test]
    async fn rejects_cursor_with_wrong_kind() {
        let state = build_state(Vec::new(), Vec::new()).await;
        let wrong = Cursor::account_deltas(5);
        let err = list_global_proposals(&state, 50, Some(wrong))
            .await
            .unwrap_err();
        assert!(matches!(err, GuardianError::InvalidCursor(_)));
    }

    #[tokio::test]
    async fn storage_failure_surfaces_as_data_unavailable() {
        use crate::ack::AckRegistry;
        use crate::builder::clock::test::MockClock;
        use crate::testing::mocks::MockNetworkClient;
        use tokio::sync::Mutex;
        let metadata = MockMetadataStore::new();
        let storage = MockStorageBackend::new()
            .with_list_global_proposals_paged(Err("storage unreachable".into()));
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
        let err = list_global_proposals(&state, 50, None).await.unwrap_err();
        assert!(matches!(err, GuardianError::DataUnavailable(_)));
    }

    /// Empty page when storage returns no rows. Verifies the
    /// thin-pass-through path with no auth lookups (no rows ⇒ no
    /// metadata.get calls).
    #[tokio::test]
    async fn empty_storage_response_returns_empty_page() {
        let state = build_state(Vec::new(), Vec::new()).await;
        let page = list_global_proposals(&state, 50, None).await.unwrap();
        assert!(page.items.is_empty());
        assert!(page.next_cursor.is_none());
    }
}
