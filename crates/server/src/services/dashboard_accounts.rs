use chrono::DateTime;
use serde::{Deserialize, Serialize};

use crate::dashboard::cursor::{self, Cursor, CursorKind};
use crate::error::{GuardianError, Result};
use crate::metadata::{AccountListCursor, AccountMetadata, auth::Auth};
use crate::services::dashboard_pagination::PagedResult;
use crate::state::AppState;
use crate::state_object::StateObject;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DashboardAccountStateStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct DashboardAccountSummary {
    pub account_id: String,
    /// Bech32m encoding of the Miden `AccountId` using the network's
    /// HRP (e.g. `mtst...`, `mdev...`, `mm...`). `None` for EVM
    /// accounts (no bech32 in that addressing scheme) and for any
    /// Miden `account_id` that fails to parse as a 15-byte id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id_bech32: Option<String>,
    pub auth_scheme: String,
    pub authorized_signer_count: usize,
    pub has_pending_candidate: bool,
    pub current_commitment: Option<String>,
    pub state_status: DashboardAccountStateStatus,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub paused_at: Option<String>,
    #[serde(default)]
    pub paused_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct DashboardAccountDetail {
    pub account_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id_bech32: Option<String>,
    pub auth_scheme: String,
    pub authorized_signer_count: usize,
    pub authorized_signer_ids: Vec<String>,
    pub has_pending_candidate: bool,
    pub current_commitment: Option<String>,
    pub state_status: DashboardAccountStateStatus,
    pub created_at: String,
    pub updated_at: String,
    pub state_created_at: Option<String>,
    pub state_updated_at: Option<String>,
    /// RFC 3339 UTC timestamp of the original pause; `None` when
    /// active. Always emitted (active accounts get `null`) for a
    /// uniform wire shape.
    #[serde(default)]
    pub paused_at: Option<String>,
    /// Reason captured at first pause; `None` when active.
    #[serde(default)]
    pub paused_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GetDashboardAccountResult {
    pub account: DashboardAccountDetail,
}

/// Paginated account list per feature `005-operator-dashboard-metrics`
/// US1 / FR-001..FR-008. Returns at most `limit` accounts ordered by
/// `(updated_at DESC, account_id ASC)` starting after `cursor`. The
/// returned `next_cursor` is `None` at end of list.
///
/// This is the v1 contract for `GET /dashboard/accounts`. The
/// unparameterized full-inventory mode and `total_count` field from
/// `003-operator-account-apis` are removed (breaking change). Aggregate
/// counts are exposed via `GET /dashboard/info` instead.
pub async fn list_dashboard_accounts_paged(
    state: &AppState,
    limit: u32,
    cursor: Option<Cursor>,
    paused: Option<bool>,
) -> Result<PagedResult<DashboardAccountSummary>> {
    if let Some(c) = cursor.as_ref()
        && c.kind != CursorKind::AccountList
    {
        return Err(GuardianError::InvalidCursor(
            "expected AccountList cursor kind".to_string(),
        ));
    }

    let storage_cursor =
        cursor
            .as_ref()
            .and_then(|c| match (c.last_updated_at, &c.last_account_id) {
                (Some(last_updated_at), Some(last_account_id)) => Some(AccountListCursor {
                    last_updated_at,
                    last_account_id: last_account_id.clone(),
                }),
                _ => None,
            });
    // Page-plus-one pattern: storage returns up to limit+1 rows so we
    // can detect end-of-list and emit a `next_cursor` only when more
    // rows exist. Postgres pushes the sort and the composite cursor
    // predicate into SQL via `idx_account_metadata_updated_at_account_id`;
    // filesystem fans out + sorts in memory.
    let page_size = limit.saturating_add(1);
    let metadatas = state
        .metadata
        .list_paged(page_size, storage_cursor, paused)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to list metadata: {e}")))?;

    let mut metadatas = metadatas;
    let limit_us = limit as usize;
    let has_more = metadatas.len() > limit_us;
    metadatas.truncate(limit_us);

    // Single batched state read instead of N round trips.
    let id_refs: Vec<&str> = metadatas.iter().map(|m| m.account_id.as_str()).collect();
    let states = state
        .storage
        .pull_states_batch(&id_refs)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to batch-pull states: {e}")))?;

    let summaries: Vec<DashboardAccountSummary> = metadatas
        .iter()
        .map(|metadata| {
            let (current_commitment, state_status) = match states.get(&metadata.account_id) {
                Some(s) => (
                    Some(s.commitment.clone()),
                    DashboardAccountStateStatus::Available,
                ),
                None => (None, DashboardAccountStateStatus::Unavailable),
            };
            DashboardAccountSummary::from_parts(metadata, current_commitment, state_status)
        })
        .collect();

    let next_cursor = if has_more {
        // When `has_more` is true and we have a last entry, the cursor
        // MUST be produced — silently falling back to `None` would
        // prematurely terminate traversal and silently drop rows. A
        // parse failure here means the stored `updated_at` is not
        // RFC3339, which is a data-integrity bug we want surfaced as
        // a 500 (StorageError) rather than a quiet truncation.
        match summaries.last() {
            Some(last) => {
                let updated_at = parse_timestamp(&last.updated_at)
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .ok_or_else(|| {
                        GuardianError::StorageError(format!(
                            "account list cursor: stored updated_at is not RFC3339 for '{}': '{}'",
                            last.account_id, last.updated_at
                        ))
                    })?;
                let cursor = Cursor::account_list(updated_at, last.account_id.clone());
                Some(cursor::encode(&cursor, state.dashboard.cursor_secret())?)
            }
            // has_more = true with no items is impossible (page_size
            // > limit_us > 0), but treat defensively as end-of-list
            // rather than panic.
            None => None,
        }
    } else {
        None
    };

    Ok(PagedResult::new(summaries, next_cursor))
}

pub async fn get_dashboard_account(
    state: &AppState,
    account_id: &str,
) -> Result<GetDashboardAccountResult> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|error| GuardianError::StorageError(format!("Failed to load metadata: {error}")))?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    let account_state = state
        .storage
        .pull_state(account_id)
        .await
        .map_err(|error| {
            tracing::warn!(
                account_id = %account_id,
                error = %error,
                "Dashboard account detail could not load state"
            );
            GuardianError::AccountDataUnavailable(account_id.to_string())
        })?;

    Ok(GetDashboardAccountResult {
        account: DashboardAccountDetail::from_parts(&metadata, &account_state),
    })
}

impl DashboardAccountSummary {
    fn from_parts(
        metadata: &AccountMetadata,
        current_commitment: Option<String>,
        state_status: DashboardAccountStateStatus,
    ) -> Self {
        Self {
            account_id: metadata.account_id.clone(),
            account_id_bech32: bech32_for_account(metadata),
            auth_scheme: metadata.auth.scheme().to_string(),
            authorized_signer_count: normalized_authorized_signer_ids(&metadata.auth).len(),
            has_pending_candidate: metadata.has_pending_candidate,
            current_commitment,
            state_status,
            created_at: metadata.created_at.clone(),
            updated_at: metadata.updated_at.clone(),
            paused_at: metadata.paused_at.map(|ts| ts.to_rfc3339()),
            paused_reason: metadata.paused_reason.clone(),
        }
    }
}

impl DashboardAccountDetail {
    fn from_parts(metadata: &AccountMetadata, account_state: &StateObject) -> Self {
        let authorized_signer_ids = normalized_authorized_signer_ids(&metadata.auth);

        Self {
            account_id: metadata.account_id.clone(),
            account_id_bech32: bech32_for_account(metadata),
            auth_scheme: metadata.auth.scheme().to_string(),
            authorized_signer_count: authorized_signer_ids.len(),
            authorized_signer_ids,
            has_pending_candidate: metadata.has_pending_candidate,
            current_commitment: Some(account_state.commitment.clone()),
            state_status: DashboardAccountStateStatus::Available,
            created_at: metadata.created_at.clone(),
            updated_at: metadata.updated_at.clone(),
            state_created_at: Some(account_state.created_at.clone()),
            state_updated_at: Some(account_state.updated_at.clone()),
            paused_at: metadata.paused_at.map(|ts| ts.to_rfc3339()),
            paused_reason: metadata.paused_reason.clone(),
        }
    }
}

/// Encode a Miden account_id (hex) as bech32m using the network HRP
/// from the account's persisted `NetworkConfig`. Returns `None` for
/// EVM accounts and for any hex that fails to parse as a Miden
/// `AccountId` (data drift). The HRP set is fixed by the Miden
/// protocol: `mm` for mainnet, `mtst` for testnet, `mdev` for devnet
/// (Local is folded into Devnet — Miden has no separate Local HRP).
fn bech32_for_account(metadata: &AccountMetadata) -> Option<String> {
    use crate::metadata::NetworkConfig;
    use miden_protocol::account::AccountId;
    let network_type = match &metadata.network_config {
        NetworkConfig::Miden { network_type } => *network_type,
        NetworkConfig::Evm { .. } => return None,
    };
    let account_id = AccountId::from_hex(&metadata.account_id).ok()?;
    Some(account_id.to_bech32(network_type.to_miden_network_id()))
}

fn normalized_authorized_signer_ids(auth: &Auth) -> Vec<String> {
    let mut signer_ids = match auth {
        Auth::MidenFalconRpo {
            cosigner_commitments,
        }
        | Auth::MidenEcdsa {
            cosigner_commitments,
        } => cosigner_commitments.clone(),
        Auth::EvmEcdsa { signers } => signers.clone(),
    };
    signer_ids.sort();
    signer_ids.dedup();
    signer_ids
}

fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::metadata::{MidenNetworkType, NetworkConfig};
    use crate::storage::filesystem::FilesystemService;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    fn miden_meta(account_id: &str, updated_at: &str) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xc1".into()],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: updated_at.to_string(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
        }
    }

    #[test]
    fn bech32_for_miden_account_encodes_with_network_hrp() {
        let mut meta = miden_meta("0x7bfb0f38b0fafa103f86a805594170", "2026-05-26T00:00:00Z");
        meta.network_config = NetworkConfig::Miden {
            network_type: MidenNetworkType::Testnet,
        };
        let bech32 = bech32_for_account(&meta).expect("bech32 encodes for valid miden id");
        assert!(
            bech32.starts_with("mtst1"),
            "testnet HRP expected, got '{bech32}'",
        );

        meta.network_config = NetworkConfig::Miden {
            network_type: MidenNetworkType::Local,
        };
        let bech32_local = bech32_for_account(&meta).expect("local maps to devnet HRP");
        assert!(
            bech32_local.starts_with("mdev1"),
            "local folds into devnet HRP, got '{bech32_local}'",
        );
    }

    #[test]
    fn bech32_for_evm_account_returns_none() {
        let meta = AccountMetadata {
            account_id: "evm:1:0xabc".to_string(),
            auth: Auth::EvmEcdsa {
                signers: vec!["0xs1".into()],
            },
            network_config: NetworkConfig::Evm {
                chain_id: 1,
                account_address: "0xabc".to_string(),
                multisig_validator_address: "0xdef".to_string(),
            },
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-26T00:00:00Z".into(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
            paused_at: None,
            paused_reason: None,
        };
        assert!(bech32_for_account(&meta).is_none());
    }

    #[test]
    fn bech32_for_unparseable_miden_account_returns_none() {
        let meta = miden_meta("not-hex", "2026-05-26T00:00:00Z");
        assert!(bech32_for_account(&meta).is_none());
    }

    /// Bug #6 regression: walk multi-page cursor traversal end-to-end
    /// against the SQL-pushed pagination path. Asserts the service
    /// pulls metadata via `list_paged` (composite cursor predicate)
    /// and batches state reads via `pull_states_batch`. Pre-fix the
    /// service called `metadata.list()` + N `metadata.get()` + N
    /// `pull_state()` per page.
    #[tokio::test]
    async fn cursor_walks_every_page_no_skip_no_repeat() {
        let dir = TempDir::new().expect("tempdir");
        let svc = FilesystemService::new(dir.path().to_path_buf())
            .await
            .expect("svc");
        // Seed 11 accounts with strictly different updated_at so the
        // composite (updated_at DESC, account_id ASC) sort is
        // unambiguous.
        let total: u64 = 11;
        let mut metas: Vec<AccountMetadata> = (0..total)
            .map(|i| {
                miden_meta(
                    &format!("acc-{i:02}"),
                    &format!("2026-05-08T12:{:02}:00Z", i),
                )
            })
            .collect();
        // newest-first
        metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

        // Mock list_paged: hand out one queued response per page.
        // limit = 4 → pages [4, 4, 3] = 11 entries; page-plus-one
        // means the service requests 5 rows on pages 1/2 and gets 5
        // back, then on page 3 it asks for 5 and gets the remaining
        // 3 (no more rows → next_cursor = None).
        let mut metadata = MockMetadataStore::new();
        // Mock LIFO: queue page 3 first, page 2 next, page 1 last.
        metadata = metadata.with_list_paged(Ok(metas[8..].to_vec()));
        metadata = metadata.with_list_paged(Ok(metas[4..9].to_vec()));
        metadata = metadata.with_list_paged(Ok(metas[0..5].to_vec()));

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
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        };

        let limit = 4;
        let mut all: Vec<String> = Vec::new();
        let mut next_cursor: Option<Cursor> = None;
        let mut pages = 0;
        for _ in 0..10 {
            let page = list_dashboard_accounts_paged(&state, limit, next_cursor, None)
                .await
                .expect("list");
            for entry in &page.items {
                all.push(entry.account_id.clone());
            }
            pages += 1;
            match page.next_cursor {
                Some(encoded) => {
                    let decoded = cursor::decode(
                        &encoded,
                        state.dashboard.cursor_secret(),
                        CursorKind::AccountList,
                    )
                    .expect("decode cursor");
                    next_cursor = Some(decoded);
                }
                None => break,
            }
        }
        assert_eq!(
            all.len(),
            total as usize,
            "every account returned exactly once"
        );
        assert_eq!(pages, 3, "ceil(11/4)");

        // Dedup-and-coverage check: every seeded id appears exactly
        // once.
        let mut seen = std::collections::HashSet::new();
        for id in &all {
            assert!(seen.insert(id.clone()), "duplicate account: {id}");
        }
    }
}
