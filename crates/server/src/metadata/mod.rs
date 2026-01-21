use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod auth;
pub mod filesystem;
#[cfg(feature = "postgres")]
pub mod postgres;

pub use auth::{Auth, AuthHeader, Credentials, ExtractCredentials};

/// Metadata for a single account
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountMetadata {
    pub account_id: String,
    pub auth: Auth,
    pub created_at: String,
    pub updated_at: String,
    pub has_pending_candidate: bool,
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
}
