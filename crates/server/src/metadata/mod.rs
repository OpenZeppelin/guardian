use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod auth;
pub mod filesystem;
pub mod network;
#[cfg(feature = "postgres")]
pub mod postgres;

pub use auth::{Auth, AuthHeader, Credentials, ExtractCredentials};
pub use network::{MidenNetworkType, NetworkConfig};

/// Metadata for a single account
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountMetadata {
    pub account_id: String,
    pub auth: Auth,
    pub network_config: NetworkConfig,
    pub created_at: String,
    pub updated_at: String,
    pub has_pending_candidate: bool,
    /// UTC timestamp of the first pause request that took effect.
    /// `None` when active. First-writer-wins: re-pause does not
    /// update this value.
    #[serde(default)]
    pub paused_at: Option<DateTime<Utc>>,
    /// Operator-supplied reason captured at first pause. `None` when
    /// active. Required (non-empty, ≤ 512 UTF-8 chars) on pause.
    #[serde(default)]
    pub paused_reason: Option<String>,
    /// UTC timestamp at which this server detected it is no longer the
    /// account's guardian (a canonicalized `SwitchGuardian` delta moved
    /// the on-chain guardian key away from this server's ack key).
    /// `None` while this server is the account's guardian. Terminal:
    /// cleared only by re-onboarding through `/configure`, which
    /// re-validates the guardian binding. First-writer-wins like
    /// `paused_at`; orthogonal to pause so an operator unpause can
    /// never resurrect a released account.
    #[serde(default)]
    pub released_at: Option<DateTime<Utc>>,
}

/// Cursor parameters for the paginated account list read. Sort key is
/// `(updated_at DESC, account_id ASC)`. The mutable `updated_at` field
/// carries the FR-005 caveat: a concurrent write that bumps an
/// account's `updated_at` mid-traversal MAY cause that entry to be
/// skipped or repeated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountListCursor {
    pub last_updated_at: DateTime<Utc>,
    pub last_account_id: String,
}

/// Metadata store trait for managing account metadata
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Connection-pool saturation snapshot for the metadata pool,
    /// which is independent of the storage pool and also backs the
    /// audit writer, operator auth, and dashboard listings. `None`
    /// (the default) for backends without a pool — the filesystem
    /// store has none.
    fn pool_status(&self) -> Option<crate::storage::PoolStatus> {
        None
    }

    /// Get metadata for a specific account
    async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String>;

    /// Store or update metadata for an account.
    ///
    /// Lifecycle fields — `has_pending_candidate`, `paused_at` /
    /// `paused_reason`, and `released_at` — are owned by their dedicated
    /// mutation methods and MUST NOT be changed by this generic write when
    /// the account already exists. Callers read the row, spend time in
    /// network calls, and write it back (e.g. `configure_account`), so a
    /// `set` that applied these fields would clobber a concurrent
    /// `submit_candidate` or pause with stale values. Shared backends
    /// enforce this in the upsert itself; the single-process filesystem
    /// store enforces it for pause/release only and accepts the residual
    /// in-process candidate-flag window. Their values still apply on first
    /// insert, where no concurrent owner can exist yet.
    async fn set(&self, metadata: AccountMetadata) -> Result<(), String>;

    /// List all account IDs
    async fn list(&self) -> Result<Vec<String>, String>;

    /// Paginated list of account metadata sorted newest-first by
    /// `(updated_at DESC, account_id ASC)`. Returns up to `limit`
    /// rows starting strictly after `cursor` (or from the beginning
    /// when `cursor` is `None`). Postgres pushes this into SQL via
    /// the composite index added in migration
    /// `2026-05-10-000002_account_metadata_pagination_index`;
    /// filesystem fans out and sorts in memory.
    ///
    /// `paused` filters by pause state when supplied: `Some(true)`
    /// returns only currently-paused accounts (served by the partial
    /// index on `paused_at`), `Some(false)` only active accounts,
    /// `None` returns all.
    async fn list_paged(
        &self,
        limit: u32,
        cursor: Option<AccountListCursor>,
        paused: Option<bool>,
    ) -> Result<Vec<AccountMetadata>, String>;

    /// Update the authentication configuration for an account
    async fn update_auth(&self, account_id: &str, new_auth: Auth, now: &str) -> Result<(), String> {
        let mut metadata = self
            .get(account_id)
            .await?
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        if metadata.auth == new_auth {
            return Ok(());
        }

        metadata.auth = new_auth;
        metadata.updated_at = now.to_string();

        self.set(metadata).await
    }

    /// Set the has_pending_candidate flag for an account
    async fn set_has_pending_candidate(
        &self,
        account_id: &str,
        has_candidate: bool,
        now: &str,
    ) -> Result<(), String> {
        let mut metadata = self
            .get(account_id)
            .await?
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        if metadata.has_pending_candidate == has_candidate {
            return Ok(());
        }

        metadata.has_pending_candidate = has_candidate;
        metadata.updated_at = now.to_string();

        self.set(metadata).await
    }

    /// Clear `has_pending_candidate` only if the account has no
    /// candidate-status delta at the moment of the write.
    ///
    /// A blind `set_has_pending_candidate(false)` is unsafe as the trailing
    /// cleanup after a promotion or discard: between the custody write that
    /// removes the candidate row and the flag clear, a new submission can pass
    /// the row-scan gate, insert a fresh candidate, and set the flag — the
    /// blind clear then clobbers it. Because the canonicalization worker
    /// selects accounts by this flag while the submission gate scans delta
    /// rows, a wrongly-cleared flag makes the account invisible to the worker
    /// *and* keeps new submissions rejected: a permanent wedge. Backends that
    /// can check the delta table in the same statement MUST override this with
    /// an atomic conditional write. This default (a plain clear) is only
    /// acceptable for single-process backends, where the residual in-process
    /// window is the pre-existing one.
    async fn clear_pending_candidate_if_none(
        &self,
        account_id: &str,
        now: &str,
    ) -> Result<(), String> {
        self.set_has_pending_candidate(account_id, false, now).await
    }

    /// List all account IDs that have pending candidates
    async fn list_with_pending_candidates(&self) -> Result<Vec<String>, String>;

    /// Atomically record the last authentication timestamp for replay protection.
    ///
    /// Compare-and-swap: records `new_timestamp` only when it is strictly greater
    /// than the stored value (a missing record accepts any timestamp and creates
    /// one). Returns `Ok(true)` when recorded, `Ok(false)` when not greater — the
    /// replay signal, which must never surface as `Err`; `Err` is reserved for
    /// storage failure. Replay state is owned exclusively by this method: no other
    /// store operation may read or write it, and it must not affect `updated_at`.
    async fn update_last_auth_timestamp_cas(
        &self,
        account_id: &str,
        new_timestamp: i64,
    ) -> Result<bool, String>;

    /// Find every account whose Miden cosigner-commitment authorization set
    /// contains the given commitment. Used by the `/state/lookup` endpoint.
    ///
    /// EVM accounts (`Auth::EvmEcdsa`) store signers in `signers` rather than
    /// `cosigner_commitments` and MUST never match.
    ///
    /// `commitment` is expected to be a `0x`-prefixed lowercase hex string;
    /// format validation is the caller's responsibility.
    async fn find_by_cosigner_commitment(&self, commitment: &str) -> Result<Vec<String>, String>;

    /// Atomically transition an account to the paused state.
    /// First-writer-wins: when the account is already paused, the
    /// persisted `paused_at` and `paused_reason` are left unchanged.
    /// Returns the `PauseTransition` describing before/after states so
    /// the handler can emit the matching audit row without a second
    /// read. Returns `Err` if the account does not exist.
    async fn set_pause(
        &self,
        account_id: &str,
        now: DateTime<Utc>,
        reason: &str,
    ) -> Result<crate::services::account_status::PauseTransition, String>;

    /// Atomically clear the pause state for an account. Idempotent: a
    /// call against an already-active account is a no-op at the
    /// persistence level and returns `before_state == after_state ==
    /// Active`. Returns `Err` if the account does not exist.
    async fn clear_pause(
        &self,
        account_id: &str,
    ) -> Result<crate::services::account_status::PauseTransition, String>;

    /// Atomically transition an account to the released state after a
    /// guardian switch away from this server was detected.
    /// First-writer-wins: when the account is already released the
    /// persisted `released_at` is left unchanged. Returns `Ok(true)`
    /// when this call performed the transition (callers audit on this),
    /// `Ok(false)` when the account was already released, `Err` if the
    /// account does not exist. Like pause, released state is owned by
    /// this method + [`Self::clear_released`]; a generic [`Self::set`]
    /// must never change it.
    async fn set_released(&self, account_id: &str, now: DateTime<Utc>) -> Result<bool, String>;

    /// Clear the released state for an account. Called only from the
    /// `/configure` re-onboarding path, which re-validates that this
    /// server is the account's guardian before reactivating it.
    /// Idempotent; `Err` if the account does not exist.
    async fn clear_released(&self, account_id: &str) -> Result<(), String>;
}
