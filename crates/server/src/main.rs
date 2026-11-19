pub use guardian_shared::{FromJson, ToJson};

use server::ack::AckRegistry;
use server::builder::{ServerBuilder, storage::StorageMetadataBuilder};
use server::canonicalization::CanonicalizationConfig;
use server::config::stage::Stage;
use server::logging::LoggingConfig;
use server::middleware::{BodyLimitConfig, CorsConfig, RateLimitConfig};
use server::network::{NetworkType, RpcSettings};
use std::env;
use std::path::PathBuf;

#[cfg(feature = "postgres")]
use server::builder::handle::StartupRecoveryError;
use server::builder::handle::StartupRecoveryMode;

const STARTUP_RECOVERY_ENV: &str = "GUARDIAN_STARTUP_ACCOUNT_RECOVERY";

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let startup_recovery = match env::var(STARTUP_RECOVERY_ENV) {
        Ok(value) => value
            .parse::<StartupRecoveryMode>()
            .unwrap_or_else(|error| {
                eprintln!("{STARTUP_RECOVERY_ENV} {error}");
                std::process::exit(2);
            }),
        Err(env::VarError::NotPresent) => StartupRecoveryMode::Off,
        Err(env::VarError::NotUnicode(_)) => {
            eprintln!("{STARTUP_RECOVERY_ENV} must be valid UTF-8");
            std::process::exit(2);
        }
    };
    #[cfg(not(feature = "postgres"))]
    if startup_recovery != StartupRecoveryMode::Off {
        eprintln!("Account recovery requires a Postgres build");
        std::process::exit(2);
    }

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

    let release_sweep = server::release_sweep::ReleaseSweepConfig::from_env()
        .expect("Invalid release sweep configuration");
    let server = ServerBuilder::new()
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
                .expect("Invalid pending-candidate queue depth configuration")
                .with_rebuild_admission_from_env()
                .expect("Invalid canonicalization rebuild admission configuration"),
        ))
        .with_execution(
            server::config::execution::ExecutionConfig::from_env()
                .expect("Invalid Guardian execution configuration"),
        )
        .with_release_sweep(Some(release_sweep.clone()))
        .with_block_watch(Some(
            server::block_watch::BlockWatchConfig::from_env_following(release_sweep.enabled)
                .expect("Invalid block watch configuration"),
        ))
        .with_rate_limit(RateLimitConfig::from_env())
        .with_body_limit(BodyLimitConfig::from_env())
        .storage(storage_backend)
        .metadata(metadata)
        .auditor(auditor)
        .coordination(coordination)
        .ack(ack)
        .http(true, 3000)
        .grpc(true, 50051)
        .cors(cors_layer)
        .build()
        .await
        .expect("Failed to build server");

    #[cfg(feature = "postgres")]
    {
        match server.recover_accounts_on_startup(startup_recovery).await {
            Ok(()) => {}
            Err(error @ StartupRecoveryError::Incomplete(_)) => {
                tracing::error!(%error, "Startup account recovery did not finish; serving without it");
            }
            Err(
                error @ (StartupRecoveryError::LeaseBusy
                | StartupRecoveryError::LeaseUnavailable(_)),
            ) => {
                tracing::error!(%error, "Cannot serve while account recovery may be active elsewhere");
                std::process::exit(1);
            }
        }
    }

    server.run().await;
}
