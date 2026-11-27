//! The server as `main` builds it, on configurable ports and without canonicalization, so pushes
//! commit optimistically and never wait on a node. For benchmarks next to a running server.
//!
//! ```text
//! BENCH_HTTP_PORT=3100 BENCH_GRPC_PORT=50151 cargo run --release -p guardian-server --example bench_server
//! ```

use server::ack::AckRegistry;
use server::builder::{ServerBuilder, storage::StorageMetadataBuilder};
use server::logging::LoggingConfig;
use server::middleware::{BodyLimitConfig, CorsConfig, RateLimitConfig};
use server::network::{NetworkType, RpcSettings};

fn port(var: &str, default: u16) -> u16 {
    std::env::var(var)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    let keystore_path: std::path::PathBuf = std::env::var("GUARDIAN_KEYSTORE_PATH")
        .expect("GUARDIAN_KEYSTORE_PATH is required")
        .into();
    let (storage_backend, metadata, auditor, coordination) = StorageMetadataBuilder::from_env()
        .build()
        .await
        .expect("Failed to initialize storage backends");
    let ack = AckRegistry::new(keystore_path)
        .await
        .expect("Failed to initialize ack registry");
    let network_type =
        NetworkType::from_env("GUARDIAN_NETWORK_TYPE").expect("Failed to resolve network type");

    ServerBuilder::new()
        .with_logging(LoggingConfig::default())
        .network(network_type)
        .with_rpc(RpcSettings::from_env(network_type).expect("Invalid RPC configuration"))
        .with_canonicalization(None)
        .with_rate_limit(RateLimitConfig::from_env())
        .with_body_limit(BodyLimitConfig::from_env())
        .storage(storage_backend)
        .metadata(metadata)
        .auditor(auditor)
        .coordination(coordination)
        .ack(ack)
        .http(true, port("BENCH_HTTP_PORT", 3100))
        .grpc(true, port("BENCH_GRPC_PORT", 50151))
        .cors(
            CorsConfig::from_env()
                .expect("Failed to initialize CORS config")
                .layer(),
        )
        .build()
        .await
        .expect("Failed to build server")
        .run()
        .await;
}
