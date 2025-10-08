use crate::metadata::file_store::FileMetadataStore;
use crate::metadata::MetadataStore;
use crate::storage::filesystem::{FilesystemConfig, FilesystemService};
use crate::storage::StorageBackend;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Initialize storage backend based on configuration
pub async fn initialize_storage() -> Result<Arc<dyn StorageBackend>, String> {
    // For now, only filesystem storage is supported
    // In the future, we can read from env var to determine storage type
    println!("Initializing filesystem storage...");
    let fs_config = FilesystemConfig::from_env()?;
    let fs_service = FilesystemService::new(fs_config).await?;

    Ok(Arc::new(fs_service))
}

/// Initialize metadata store
pub async fn initialize_metadata() -> Result<Arc<Mutex<dyn MetadataStore>>, String> {
    println!("Initializing metadata store...");
    let fs_config = FilesystemConfig::from_env()?;
    let metadata_store = FileMetadataStore::new(fs_config.app_path).await?;

    Ok(Arc::new(Mutex::new(metadata_store)))
}
