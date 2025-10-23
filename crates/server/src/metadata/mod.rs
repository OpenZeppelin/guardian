use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod auth;
pub mod filesystem;

pub use auth::{Auth, AuthHeader, Credentials, ExtractCredentials};

use crate::storage::StorageType;

/// Metadata for a single account
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountMetadata {
    pub account_id: String,
    pub auth: Auth,
    pub storage_type: StorageType,
    pub created_at: String,
    pub updated_at: String,
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
    async fn update_auth(
        &self,
        account_id: &str,
        new_auth: Auth,
        now: &str,
    ) -> Result<(), String> {
        let mut metadata = self
            .get(account_id)
            .await?
            .ok_or_else(|| format!("Account not found: {}", account_id))?;

        if metadata.auth == new_auth {
            return Ok(());
        }

        metadata.auth = new_auth;
        metadata.updated_at = now.to_string();

        self.set(metadata).await
    }
}
