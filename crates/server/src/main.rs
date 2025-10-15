pub use private_state_manager_shared::{FromJson, ToJson};

use server::builder::ServerBuilder;
use server::network::NetworkType;
use server::storage::filesystem::{FilesystemMetadataStore, FilesystemService};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    // Get storage path from environment or use default
    let storage_path: PathBuf = env::var("PSM_STORAGE_PATH")
        .unwrap_or_else(|_| "/var/psm/storage".to_string())
        .into();

    // Get metadata path from environment or use default
    let metadata_path: PathBuf = env::var("PSM_METADATA_PATH")
        .unwrap_or_else(|_| "/var/psm/metadata".to_string())
        .into();

    // Create storage and metadata stores
    let storage = FilesystemService::new(storage_path)
        .await
        .expect("Failed to initialize filesystem storage");

    let metadata = FilesystemMetadataStore::new(metadata_path)
        .await
        .expect("Failed to initialize metadata store");

    // Build and run server
    ServerBuilder::new()
        .network(NetworkType::Miden)
        .storage(Arc::new(storage))
        .metadata(Arc::new(metadata))
        .http(true, 3000)
        .grpc(true, 50051)
        .build()
        .expect("Failed to build server")
        .run()
        .await;
}
