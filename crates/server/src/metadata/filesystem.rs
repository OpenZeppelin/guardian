use crate::metadata::{AccountMetadata, MetadataStore};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Filesystem-based metadata store
/// Stores all account metadata in a single JSON file with in-memory cache
pub struct FilesystemMetadataStore {
    file_path: PathBuf,
    /// In-memory cache of account metadata
    cache: Arc<RwLock<HashMap<String, AccountMetadata>>>,
}

impl FilesystemMetadataStore {
    /// Create a new FilesystemMetadataStore
    pub async fn new(base_path: PathBuf) -> Result<Self, String> {
        let metadata_dir = base_path.join(".metadata");
        fs::create_dir_all(&metadata_dir)
            .await
            .map_err(|e| format!("Failed to create metadata directory: {e}"))?;

        let file_path = metadata_dir.join("accounts.json");

        let cache = if file_path.exists() {
            let content = fs::read_to_string(&file_path)
                .await
                .map_err(|e| format!("Failed to read metadata file: {e}"))?;

            let accounts: HashMap<String, AccountMetadata> = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse metadata file: {e}"))?;

            Arc::new(RwLock::new(accounts))
        } else {
            Arc::new(RwLock::new(HashMap::new()))
        };

        Ok(Self { file_path, cache })
    }

    /// Persist metadata cache to disk
    async fn persist(&self, cache: &HashMap<String, AccountMetadata>) -> Result<(), String> {
        // Ensure metadata directory exists
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("Failed to create metadata directory: {e}"))?;
        }

        let content = serde_json::to_string_pretty(cache)
            .map_err(|e| format!("Failed to serialize metadata: {e}"))?;

        // Atomic write using temp file
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp_path = self.file_path.with_extension(format!(
            "tmp.{}.{}.{}",
            std::process::id(),
            nanos,
            counter
        ));
        let mut file = fs::File::create(&temp_path)
            .await
            .map_err(|e| format!("Failed to create temp file: {e}"))?;

        file.write_all(content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write to temp file: {e}"))?;

        file.sync_all()
            .await
            .map_err(|e| format!("Failed to sync temp file: {e}"))?;

        drop(file);

        fs::rename(&temp_path, &self.file_path)
            .await
            .map_err(|e| format!("Failed to rename temp file: {e}"))?;

        Ok(())
    }
}

#[async_trait]
impl MetadataStore for FilesystemMetadataStore {
    async fn get(&self, account_id: &str) -> Result<Option<AccountMetadata>, String> {
        let cache = self.cache.read().await;
        Ok(cache.get(account_id).cloned())
    }

    async fn set(&self, metadata: AccountMetadata) -> Result<(), String> {
        let account_id = metadata.account_id.clone();

        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(account_id, metadata);
        }

        // Persist to disk
        let cache = self.cache.read().await;
        self.persist(&cache).await
    }

    async fn list(&self) -> Result<Vec<String>, String> {
        let cache = self.cache.read().await;
        Ok(cache.keys().cloned().collect())
    }

    async fn list_with_pending_candidates(&self) -> Result<Vec<String>, String> {
        let cache = self.cache.read().await;
        Ok(cache
            .iter()
            .filter(|(_, m)| m.has_pending_candidate)
            .map(|(k, _)| k.clone())
            .collect())
    }

    async fn update_last_auth_timestamp_cas(
        &self,
        account_id: &str,
        new_timestamp: i64,
        now: &str,
    ) -> Result<bool, String> {
        let mut cache = self.cache.write().await;

        let metadata = cache
            .get_mut(account_id)
            .ok_or_else(|| format!("Account not found: {account_id}"))?;

        if let Some(current) = metadata.last_auth_timestamp
            && new_timestamp <= current
        {
            return Ok(false); // Potential replay, don't update
        }

        metadata.last_auth_timestamp = Some(new_timestamp);
        metadata.updated_at = now.to_string();

        self.persist(&cache).await?;
        Ok(true)
    }
}
