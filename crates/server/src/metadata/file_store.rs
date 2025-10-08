use super::{AccountMetadata, AccountsMetadata, MetadataStore};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

/// File-based metadata store that keeps all Accounts metadata in a single JSON file
pub struct FileMetadataStore {
    file_path: PathBuf,
    data: Arc<RwLock<AccountsMetadata>>,
}

impl FileMetadataStore {
    /// Create a new FileMetadataStore
    pub async fn new(base_path: PathBuf) -> Result<Self, String> {
        let metadata_dir = base_path.join(".metadata");
        fs::create_dir_all(&metadata_dir)
            .await
            .map_err(|e| format!("Failed to create metadata directory: {}", e))?;

        let file_path = metadata_dir.join("accounts.json");

        let data = if file_path.exists() {
            let content = fs::read_to_string(&file_path)
                .await
                .map_err(|e| format!("Failed to read metadata file: {}", e))?;

            serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse metadata file: {}", e))?
        } else {
            AccountsMetadata::default()
        };

        Ok(Self {
            file_path,
            data: Arc::new(RwLock::new(data)),
        })
    }

    /// Atomically write metadata to disk
    async fn write(&self, data: &AccountsMetadata) -> Result<(), String> {
        let content = serde_json::to_string_pretty(data)
            .map_err(|e| format!("Failed to serialize metadata: {}", e))?;

        // Write to temp file first to ensure atomic operation:
        // If process crashes during write, original file remains intact.
        // The rename operation below is atomic on Unix/Linux.
        let temp_path = self.file_path.with_extension("tmp");
        let mut file = fs::File::create(&temp_path)
            .await
            .map_err(|e| format!("Failed to create temp file: {}", e))?;

        file.write_all(content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to temp file: {}", e))?;

        file.sync_all()
            .await
            .map_err(|e| format!("Failed to sync temp file: {}", e))?;

        drop(file);

        fs::rename(&temp_path, &self.file_path)
            .await
            .map_err(|e| format!("Failed to rename temp file: {}", e))?;

        Ok(())
    }
}

#[async_trait]
impl MetadataStore for FileMetadataStore {
    async fn get_account(&self, account_id: &str) -> Result<Option<AccountMetadata>, String> {
        let data = self.data.read().await;
        Ok(data.accounts.get(account_id).cloned())
    }

    async fn set_account(&mut self, metadata: AccountMetadata) -> Result<(), String> {
        let account_id = metadata.account_id.clone();

        // Scope to ensure write lock is released before acquiring read lock below
        {
            let mut data = self.data.write().await;
            data.accounts.insert(account_id, metadata);
        } 

        let data = self.data.read().await;
        self.write(&*data).await
    }

    async fn remove_account(&mut self, account_id: &str) -> Result<(), String> {
        // Scope to ensure write lock is released before acquiring read lock below
        {
            let mut data = self.data.write().await;
            data.accounts.remove(account_id);
        }

        let data = self.data.read().await;
        self.write(&*data).await
    }

    async fn list_accounts(&self) -> Result<Vec<String>, String> {
        let data = self.data.read().await;
        Ok(data.accounts.keys().cloned().collect())
    }

    async fn save(&self) -> Result<(), String> {
        let data = self.data.read().await;
        self.write(&*data).await
    }
}
