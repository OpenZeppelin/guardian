pub use private_state_manager_shared::{FromJson, ToJson};

use server::ack::AckRegistry;
use server::builder::{ServerBuilder, storage::StorageMetadataBuilder};
use server::canonicalization::CanonicalizationConfig;
use server::logging::LoggingConfig;
use server::middleware::{BodyLimitConfig, RateLimitConfig};
use server::network::NetworkType;
use std::env;
use std::path::PathBuf;
use tower_http::cors::{Any, CorsLayer};

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let keystore_path: PathBuf = env::var("PSM_KEYSTORE_PATH")
        .unwrap_or_else(|_| "/var/psm/keystore".to_string())
        .into();

    let (storage_backend, metadata) = StorageMetadataBuilder::from_env()
        .build()
        .await
        .expect("Failed to initialize storage backends");

    let ack = AckRegistry::new(keystore_path).expect("Failed to initialize ack registry");

    let cors_layer = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    ServerBuilder::new()
        .with_logging(LoggingConfig::default())
        .network(NetworkType::MidenLocal)
        .with_canonicalization(Some(CanonicalizationConfig::new(10, 18)))
        .with_rate_limit(RateLimitConfig::from_env())
        .with_body_limit(BodyLimitConfig::from_env())
        .storage(storage_backend)
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
