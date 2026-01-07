pub use private_state_manager_shared::{FromJson, ToJson};

use server::ack::{Acknowledger, MidenFalconRpoSigner};
use server::builder::ServerBuilder;
use server::canonicalization::CanonicalizationConfig;
use server::logging::LoggingConfig;
use server::metadata::filesystem::FilesystemMetadataStore;
use server::network::NetworkType;
use server::storage::StorageRegistry;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

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

    // Initialize acknowledger
    let signer = MidenFalconRpoSigner::new(keystore_path).expect("Failed to initialize signer");
    let ack = Acknowledger::FilesystemMidenFalconRpo(signer);

    let cors_layer = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    ServerBuilder::new()
        .with_logging(LoggingConfig::default())
        .network(NetworkType::MidenTestnet)
        .with_canonicalization(Some(CanonicalizationConfig::new(10, 18)))
        .storage(storage_registry)
        .metadata(Arc::new(metadata))
        .ack(ack)
        .http(true, 3000)
        .grpc(true, 50051)
        .cors(cors_layer)
        .build()
        .await
        .expect("Failed to build server")
        .run()
        .await;
}
