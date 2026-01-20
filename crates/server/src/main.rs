pub use private_state_manager_shared::{FromJson, ToJson};

use server::ack::{Acknowledger, MidenFalconRpoSigner};
use server::builder::ServerBuilder;
use server::canonicalization::CanonicalizationConfig;
use server::logging::LoggingConfig;
use server::metadata::{filesystem::FilesystemMetadataStore, MetadataStore};
use server::network::NetworkType;
use server::storage::StorageRegistry;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
#[cfg(feature = "postgres")]
use server::metadata::postgres::PostgresMetadataStore;

#[tokio::main]
async fn main() {
    // Load .env file from current directory or parent directories
    dotenvy::dotenv().ok();

    let keystore_path: PathBuf = env::var("PSM_KEYSTORE_PATH")
        .unwrap_or_else(|_| "/var/psm/keystore".to_string())
        .into();

    let (storage_registry, metadata) = init_storage_and_metadata()
        .await
        .expect("Failed to initialize storage backends");

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
        .metadata(metadata)
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

async fn init_storage_and_metadata(
) -> Result<(StorageRegistry, Arc<dyn MetadataStore>), String> {
    #[cfg(feature = "postgres")]
    {
        let database_url = env::var("DATABASE_URL")
            .map_err(|_| "DATABASE_URL environment variable is required".to_string())?;
        let storage_registry = StorageRegistry::with_postgres(&database_url).await?;
        let metadata = PostgresMetadataStore::new(&database_url).await?;

        Ok((storage_registry, Arc::new(metadata)))
    }
    #[cfg(not(feature = "postgres"))]
    {
        let storage_path: PathBuf = env::var("PSM_STORAGE_PATH")
            .unwrap_or_else(|_| "/var/psm/storage".to_string())
            .into();
        let metadata_path: PathBuf = env::var("PSM_METADATA_PATH")
            .unwrap_or_else(|_| "/var/psm/metadata".to_string())
            .into();

        let storage_registry = StorageRegistry::with_filesystem(storage_path).await?;
        let metadata = FilesystemMetadataStore::new(metadata_path).await?;

        Ok((storage_registry, Arc::new(metadata)))
    }
}
