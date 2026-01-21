use crate::metadata::MetadataStore;
use crate::storage::StorageBackend;
use std::path::PathBuf;
use std::sync::Arc;

/// Builder for creating the storage backend and metadata store.
#[derive(Default)]
pub struct StorageMetadataBuilder {
    storage_path: Option<PathBuf>,
    metadata_path: Option<PathBuf>,
    database_url: Option<String>,
}

impl StorageMetadataBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn storage_path(mut self, path: PathBuf) -> Self {
        self.storage_path = Some(path);
        self
    }

    pub fn metadata_path(mut self, path: PathBuf) -> Self {
        self.metadata_path = Some(path);
        self
    }

    pub fn database_url(mut self, url: String) -> Self {
        self.database_url = Some(url);
        self
    }

    pub fn from_env() -> Self {
        Self::new()
            .storage_path(
                std::env::var("PSM_STORAGE_PATH")
                    .unwrap_or_else(|_| "/var/psm/storage".to_string())
                    .into(),
            )
            .metadata_path(
                std::env::var("PSM_METADATA_PATH")
                    .unwrap_or_else(|_| "/var/psm/metadata".to_string())
                    .into(),
            )
            .database_url(std::env::var("DATABASE_URL").ok().unwrap_or_default())
    }

    pub async fn build(self) -> Result<(Arc<dyn StorageBackend>, Arc<dyn MetadataStore>), String> {
        #[cfg(feature = "postgres")]
        {
            let database_url = self
                .database_url
                .filter(|url| !url.is_empty())
                .ok_or_else(|| "DATABASE_URL environment variable is required".to_string())?;

            crate::storage::postgres::run_migrations(&database_url).await?;
            let storage = crate::storage::postgres::PostgresService::new(&database_url).await?;
            let metadata =
                crate::metadata::postgres::PostgresMetadataStore::new(&database_url).await?;

            Ok((Arc::new(storage), Arc::new(metadata)))
        }

        #[cfg(not(feature = "postgres"))]
        {
            let storage_path = self
                .storage_path
                .ok_or_else(|| "PSM_STORAGE_PATH is required".to_string())?;
            let metadata_path = self
                .metadata_path
                .ok_or_else(|| "PSM_METADATA_PATH is required".to_string())?;

            let storage = crate::storage::filesystem::FilesystemService::new(storage_path).await?;
            let metadata =
                crate::metadata::filesystem::FilesystemMetadataStore::new(metadata_path).await?;

            Ok((Arc::new(storage), Arc::new(metadata)))
        }
    }
}
