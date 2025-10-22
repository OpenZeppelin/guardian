pub use private_state_manager_shared::{FromJson, ToJson};

use server::builder::ServerBuilder;
use server::canonicalization::{CanonicalizationConfig, CanonicalizationMode};
use server::logging::LoggingConfig;
use server::network::NetworkType;
use server::storage::StorageRegistry;
use server::storage::filesystem::FilesystemMetadataStore;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let storage_path: PathBuf = env::var("PSM_STORAGE_PATH")
        .unwrap_or_else(|_| "/var/psm/storage".to_string())
        .into();

    let metadata_path: PathBuf = env::var("PSM_METADATA_PATH")
        .unwrap_or_else(|_| "/var/psm/metadata".to_string())
        .into();

    let keystore_path: PathBuf = env::var("PSM_KEYSTORE_PATH")
        .unwrap_or_else(|_| "/var/psm/keystore".to_string())
        .into();

    // Create storage registry with filesystem backend
    let storage_registry = StorageRegistry::with_filesystem(storage_path)
        .await
        .expect("Failed to initialize storage registry");

    let metadata = FilesystemMetadataStore::new(metadata_path)
        .await
        .expect("Failed to initialize metadata store");

    // Set rules for canonicalization
    let canonicalization_mode = CanonicalizationMode::Enabled(CanonicalizationConfig::default());

    ServerBuilder::new()
        .with_logging(LoggingConfig::default())
        .network(NetworkType::MidenTestnet)
        .with_canonicalization(canonicalization_mode)
        .storage(storage_registry)
        .metadata(Arc::new(metadata))
        .keystore(keystore_path)
        .http(true, 3000)
        .grpc(true, 50051)
        .build()
        .await
        .expect("Failed to build server")
        .run()
        .await;
}
