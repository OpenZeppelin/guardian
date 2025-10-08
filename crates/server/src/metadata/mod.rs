use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::handlers::configure::ConfigureRequest;

pub mod file_store;

/// Metadata for a single account
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AccountMetadata {
    pub account_id: String,
    pub storage_type: String,
    pub cosigner_pubkeys: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<&ConfigureRequest> for AccountMetadata {
    fn from(request: &ConfigureRequest) -> Self {
        Self {
            account_id: request.account_id.clone(),
            storage_type: request.storage_type.clone(),
            cosigner_pubkeys: request.cosigner_pubkeys.clone(),
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// Accounts metadata that keeps all accounts metadata in a single JSON file
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct AccountsMetadata {
    pub accounts: HashMap<String, AccountMetadata>,
}

/// Trait for metadata storage operations
#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// Get metadata for a specific account
    async fn get_account(&self, account_id: &str) -> Result<Option<AccountMetadata>, String>;

    /// Store or update metadata for an account
    async fn set_account(&mut self, metadata: AccountMetadata) -> Result<(), String>;

    /// Remove metadata for an account
    async fn remove_account(&mut self, account_id: &str) -> Result<(), String>;

    /// List all account IDs
    async fn list_accounts(&self) -> Result<Vec<String>, String>;

    /// Persist metadata to storage
    async fn save(&self) -> Result<(), String>;
}
