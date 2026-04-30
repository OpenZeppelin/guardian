pub use guardian_shared::{FromJson, ToJson};

use server::ack::AckRegistry;
use server::builder::{ServerBuilder, storage::StorageMetadataBuilder};
use server::canonicalization::CanonicalizationConfig;
use server::logging::LoggingConfig;
use server::middleware::{BodyLimitConfig, CorsConfig, RateLimitConfig};
use server::network::NetworkType;
use std::env;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let keystore_path: PathBuf = env::var("GUARDIAN_KEYSTORE_PATH")
        .unwrap_or_else(|_| "/var/guardian/keystore".to_string())
        .into();

    let (storage_backend, metadata) = StorageMetadataBuilder::from_env()
        .build()
        .await
        .expect("Failed to initialize storage backends");

    // Initialize acknowledger registry (supports both Falcon and ECDSA)
    let ack = AckRegistry::new(keystore_path)
        .await
        .expect("Failed to initialize ack registry");

    let cors_layer = CorsConfig::from_env()
        .expect("Failed to initialize CORS config")
        .layer();

    let network_type = NetworkType::from_env_or("GUARDIAN_NETWORK_TYPE", NetworkType::MidenDevnet);

    ServerBuilder::new()
        .with_logging(LoggingConfig::default())
        .network(network_type)
        .with_canonicalization(Some(
            CanonicalizationConfig::new(10, 48).with_submission_grace_period_seconds(600),
        ))
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
