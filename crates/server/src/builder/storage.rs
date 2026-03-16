use std::path::PathBuf;
use std::sync::Arc;

use crate::metadata::MetadataStore;
#[cfg(not(feature = "postgres"))]
use crate::metadata::filesystem::FilesystemMetadataStore;
#[cfg(feature = "postgres")]
use crate::metadata::postgres::PostgresMetadataStore;
use crate::storage::StorageBackend;
#[cfg(not(feature = "postgres"))]
use crate::storage::filesystem::FilesystemService;
#[cfg(feature = "postgres")]
use crate::storage::postgres::{self, PostgresService};

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

            postgres::run_migrations(&database_url).await?;
            let storage = PostgresService::new(&database_url).await?;
            let metadata = PostgresMetadataStore::new(&database_url).await?;

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

            let storage = FilesystemService::new(storage_path).await?;
            let metadata = FilesystemMetadataStore::new(metadata_path).await?;

            Ok((Arc::new(storage), Arc::new(metadata)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_empty_builder() {
        let builder = StorageMetadataBuilder::new();
        assert!(builder.storage_path.is_none());
        assert!(builder.metadata_path.is_none());
        assert!(builder.database_url.is_none());
    }

    #[test]
    fn test_default_creates_empty_builder() {
        let builder = StorageMetadataBuilder::default();
        assert!(builder.storage_path.is_none());
        assert!(builder.metadata_path.is_none());
        assert!(builder.database_url.is_none());
    }

    #[test]
    fn test_storage_path_sets_path() {
        let path = PathBuf::from("/test/storage");
        let builder = StorageMetadataBuilder::new().storage_path(path.clone());
        assert_eq!(builder.storage_path, Some(path));
    }

    #[test]
    fn test_metadata_path_sets_path() {
        let path = PathBuf::from("/test/metadata");
        let builder = StorageMetadataBuilder::new().metadata_path(path.clone());
        assert_eq!(builder.metadata_path, Some(path));
    }

    #[test]
    fn test_database_url_sets_url() {
        let url = "postgres://localhost/test".to_string();
        let builder = StorageMetadataBuilder::new().database_url(url.clone());
        assert_eq!(builder.database_url, Some(url));
    }

    #[test]
    fn test_builder_chaining() {
        let storage_path = PathBuf::from("/test/storage");
        let metadata_path = PathBuf::from("/test/metadata");
        let database_url = "postgres://localhost/test".to_string();

        let builder = StorageMetadataBuilder::new()
            .storage_path(storage_path.clone())
            .metadata_path(metadata_path.clone())
            .database_url(database_url.clone());

        assert_eq!(builder.storage_path, Some(storage_path));
        assert_eq!(builder.metadata_path, Some(metadata_path));
        assert_eq!(builder.database_url, Some(database_url));
    }

    #[test]
    fn test_from_env_returns_builder_with_paths() {
        // Test that from_env returns a builder with storage_path and metadata_path set
        // We can't reliably test specific values due to env var state from other tests
        let builder = StorageMetadataBuilder::from_env();

        assert!(builder.storage_path.is_some());
        assert!(builder.metadata_path.is_some());
        assert!(builder.database_url.is_some());
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn test_build_without_storage_path_fails() {
        let builder = StorageMetadataBuilder::new().metadata_path(PathBuf::from("/test/metadata"));

        let result = builder.build().await;
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), "PSM_STORAGE_PATH is required");
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn test_build_without_metadata_path_fails() {
        let builder = StorageMetadataBuilder::new().storage_path(PathBuf::from("/test/storage"));

        let result = builder.build().await;
        assert!(result.is_err());
        assert_eq!(result.err().unwrap(), "PSM_METADATA_PATH is required");
    }

    #[cfg(not(feature = "postgres"))]
    #[tokio::test]
    async fn test_build_with_valid_paths_succeeds() {
        let temp_dir = std::env::temp_dir().join(format!("psm_test_{}", uuid::Uuid::new_v4()));
        let storage_path = temp_dir.join("storage");
        let metadata_path = temp_dir.join("metadata");

        let builder = StorageMetadataBuilder::new()
            .storage_path(storage_path.clone())
            .metadata_path(metadata_path.clone());

        let result = builder.build().await;
        assert!(result.is_ok());

        std::fs::remove_dir_all(&temp_dir).ok();
    }

    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn test_build_without_database_url_fails() {
        let builder = StorageMetadataBuilder::new();

        let result = builder.build().await;
        assert!(result.is_err());
        assert_eq!(
            result.err().unwrap(),
            "DATABASE_URL environment variable is required"
        );
    }

    #[cfg(feature = "postgres")]
    #[tokio::test]
    async fn test_build_with_empty_database_url_fails() {
        let builder = StorageMetadataBuilder::new().database_url(String::new());

        let result = builder.build().await;
        assert!(result.is_err());
        assert_eq!(
            result.err().unwrap(),
            "DATABASE_URL environment variable is required"
        );
    }
}
