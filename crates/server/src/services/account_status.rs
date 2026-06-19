//! Per-account pause chokepoint.
//!
//! Single helper [`ensure_account_active`] consulted from every
//! per-account mutating entry point (multisig + EVM proposal pipelines).
//! Admin/setup paths (`services::configure_account`,
//! `evm::service::register_account`) deliberately do NOT call this
//! helper. This module is the ONLY place outside read endpoints +
//! the pause/unpause handlers that reads `AccountMetadata::paused_at` —
//! keeping the read centralized is what lets the future `PolicyEngine`
//! replace this helper wholesale without API, audit, or storage churn.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{GuardianError, Result};
use crate::metadata::AccountMetadata;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    Active,
    Paused,
}

impl AccountStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
        }
    }
}

/// Outcome of a pause/unpause transition. The `before_state` /
/// `after_state` pair encodes idempotent retries (FR-019): a re-pause
/// of an already-paused account produces `(Paused, Paused)` with the
/// original `paused_at` preserved.
#[derive(Debug, Clone)]
pub struct PauseTransition {
    pub before_state: AccountStatus,
    pub after_state: AccountStatus,
    pub paused_at: Option<DateTime<Utc>>,
    pub paused_reason: Option<String>,
}

/// Returns `Ok(())` if the supplied metadata describes an active
/// account, or `GuardianError::AccountPaused { .. }` carrying the
/// persisted `paused_at` / `paused_reason` when the account is paused.
///
/// Callers MUST invoke this only after authentication has succeeded —
/// otherwise unauthenticated probes can learn pause state and reason.
/// Pair with `resolve_account` (multisig) or post-signature verification
/// (EVM) so the chokepoint reads from already-loaded metadata.
pub fn ensure_account_active_metadata(metadata: &AccountMetadata) -> Result<()> {
    if let Some(paused_at) = metadata.paused_at {
        return Err(GuardianError::AccountPaused {
            paused_at,
            paused_reason: metadata.paused_reason.clone(),
        });
    }
    Ok(())
}

/// Convenience wrapper that loads metadata then delegates to
/// [`ensure_account_active_metadata`]. Prefer the metadata-only form when the
/// caller has already loaded metadata via `resolve_account` or
/// `load_evm_metadata` — that path avoids a redundant DB read AND
/// keeps the pause check behind authentication.
pub async fn ensure_account_active(state: &AppState, account_id: &str) -> Result<()> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| GuardianError::StorageError(format!("Failed to load metadata: {e}")))?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    ensure_account_active_metadata(&metadata)
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;
    use crate::ack::AckRegistry;
    use crate::builder::clock::test::MockClock;
    use crate::metadata::{AccountMetadata, Auth, NetworkConfig};
    use crate::storage::filesystem::FilesystemService;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient};
    use chrono::TimeZone;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::Mutex;

    fn meta(account_id: &str, paused: Option<(DateTime<Utc>, Option<&str>)>) -> AccountMetadata {
        AccountMetadata {
            account_id: account_id.to_string(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xc1".into()],
            },
            network_config: NetworkConfig::miden_default(),
            created_at: "2026-05-01T00:00:00Z".into(),
            updated_at: "2026-05-01T00:00:00Z".into(),
            has_pending_candidate: false,
            last_auth_timestamp: None,
            paused_at: paused.map(|(ts, _)| ts),
            paused_reason: paused.and_then(|(_, r)| r.map(|s| s.to_string())),
        }
    }

    async fn state_with(metadata: MockMetadataStore) -> AppState {
        let dir = TempDir::new().expect("tempdir");
        let storage = FilesystemService::new(dir.path().to_path_buf())
            .await
            .expect("svc");
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
            auditor: Arc::new(crate::audit::LogAuditor::new()),
            #[cfg(feature = "evm")]
            evm: Arc::new(crate::evm::EvmAppState::for_tests()),
        }
    }

    #[tokio::test]
    async fn active_account_returns_ok() {
        let metadata = MockMetadataStore::new().with_get(Ok(Some(meta("acc-1", None))));
        let state = state_with(metadata).await;

        ensure_account_active(&state, "acc-1")
            .await
            .expect("active account must pass the chokepoint");
    }

    #[tokio::test]
    async fn paused_account_returns_account_paused_with_details() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 19, 14, 30, 0).unwrap();
        let metadata = MockMetadataStore::new()
            .with_get(Ok(Some(meta("acc-1", Some((ts, Some("compliance")))))));
        let state = state_with(metadata).await;

        let err = ensure_account_active(&state, "acc-1")
            .await
            .expect_err("paused account must be rejected");

        match err {
            GuardianError::AccountPaused {
                paused_at,
                paused_reason,
            } => {
                assert_eq!(paused_at, ts);
                assert_eq!(paused_reason.as_deref(), Some("compliance"));
            }
            other => panic!("expected AccountPaused, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn paused_without_reason_surfaces_none() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 19, 14, 30, 0).unwrap();
        let metadata = MockMetadataStore::new().with_get(Ok(Some(meta("acc-1", Some((ts, None))))));
        let state = state_with(metadata).await;

        match ensure_account_active(&state, "acc-1").await {
            Err(GuardianError::AccountPaused { paused_reason, .. }) => {
                assert!(paused_reason.is_none());
            }
            other => panic!("expected AccountPaused with None reason, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_account_returns_not_found() {
        let metadata = MockMetadataStore::new().with_get(Ok(None));
        let state = state_with(metadata).await;

        match ensure_account_active(&state, "missing").await {
            Err(GuardianError::AccountNotFound(id)) => assert_eq!(id, "missing"),
            other => panic!("expected AccountNotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn metadata_load_failure_surfaces_as_storage_error() {
        let metadata = MockMetadataStore::new().with_get(Err("disk on fire".to_string()));
        let state = state_with(metadata).await;

        match ensure_account_active(&state, "acc-1").await {
            Err(GuardianError::StorageError(msg)) => assert!(msg.contains("disk on fire")),
            other => panic!("expected StorageError, got {other:?}"),
        }
    }
}
