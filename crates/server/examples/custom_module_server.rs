//! The Guardian server with a policy module kind compiled in: `note-policy` runs in this
//! process, so a catalog entry can name it with `kind = "note-policy"` and no endpoint.
//! Everything else matches the `server` binary. Walkthrough in
//! `docs/guides/policy-modules/custom.md`.
//!
//! ```text
//! cargo run -p guardian-server --example custom_module_server
//! ```
//!
//! `GUARDIAN_EXAMPLE_HTTP_PORT` and `GUARDIAN_EXAMPLE_GRPC_PORT` (default 3000 and 50051) let it
//! run next to another server.

#[path = "../../guardian-module-sdk/examples/note_policy/mod.rs"]
mod note_policy;

use std::sync::Arc;

use server::ack::AckRegistry;
use server::builder::{ServerBuilder, storage::StorageMetadataBuilder};
use server::canonicalization::CanonicalizationConfig;
use server::config::stage::Stage;
use server::logging::LoggingConfig;
use server::middleware::{BodyLimitConfig, CorsConfig, RateLimitConfig};
use server::network::{NetworkType, RpcSettings};
use std::env;
use std::path::PathBuf;

fn port(var: &str, default: u16) -> u16 {
    env::var(var)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let keystore_path: PathBuf = env::var("GUARDIAN_KEYSTORE_PATH")
        .unwrap_or_else(|_| "/var/guardian/keystore".to_string())
        .into();

    let (storage_backend, metadata, auditor, coordination) = StorageMetadataBuilder::from_env()
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

    let network_type =
        NetworkType::from_env("GUARDIAN_NETWORK_TYPE").expect("Failed to resolve network type");

    let stage = Stage::from_env().expect("Invalid GUARDIAN_ENV");

    ServerBuilder::new()
        .with_logging(LoggingConfig::default())
        .network(network_type)
        .with_rpc(RpcSettings::from_env(network_type).expect("Invalid RPC configuration"))
        .with_canonicalization(Some(
            CanonicalizationConfig::new(10, 48)
                .with_stage_defaults(stage)
                .with_submission_grace_period_seconds(600)
                .with_fast_promotion_enabled_from_env()
                .expect("Invalid fast promotion configuration")
                .with_max_concurrent_accounts_from_env()
                .expect("Invalid canonicalization concurrency configuration")
                .with_retained_ttl_seconds_from_env()
                .expect("Invalid retained TTL configuration")
                .with_reconcile_interval_seconds_from_env()
                .expect("Invalid reconcile interval configuration")
                .with_max_pending_candidates_per_account_from_env()
                .expect("Invalid pending-candidate queue depth configuration"),
        ))
        .with_release_sweep(Some(
            server::release_sweep::ReleaseSweepConfig::from_env()
                .expect("Invalid release sweep configuration"),
        ))
        .with_rate_limit(RateLimitConfig::from_env())
        .with_body_limit(BodyLimitConfig::from_env())
        .storage(storage_backend)
        .metadata(metadata)
        .auditor(auditor)
        .coordination(coordination)
        .ack(ack)
        .with_module_kind(note_policy::KIND, Arc::new(note_policy::NotePolicyFactory))
        .http(true, port("GUARDIAN_EXAMPLE_HTTP_PORT", 3000))
        .grpc(true, port("GUARDIAN_EXAMPLE_GRPC_PORT", 50051))
        .cors(cors_layer)
        .build()
        .await
        .expect("Failed to build server")
        .run()
        .await;
}
