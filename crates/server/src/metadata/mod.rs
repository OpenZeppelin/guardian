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
    #[serde(default)]
    pub last_auth_timestamp: Option<i64>,
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
    /// Get metadata for a specific account
    async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String>;

    /// Store or update metadata for an account
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
    async fn list_paged(
        &self,
        limit: u32,
        cursor: Option<AccountListCursor>,
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

    /// List all account IDs that have pending candidates
    async fn list_with_pending_candidates(&self) -> Result<Vec<String>, String>;

    /// Atomically update the last authentication timestamp for replay protection.
    ///
    /// Uses compare-and-swap semantics: only updates if the new timestamp is strictly
    /// greater than the current stored timestamp. Returns Ok(true) if updated,
    /// Ok(false) if the timestamp was not greater (potential replay), or Err on failure.
    async fn update_last_auth_timestamp_cas(
        &self,
        account_id: &str,
        new_timestamp: i64,
        now: &str,
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
}
