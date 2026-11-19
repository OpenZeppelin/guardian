//! The server configured from the environment, as the official binary runs it.

use std::path::PathBuf;

use crate::ack::AckRegistry;
use crate::block_watch::BlockWatchConfig;
use crate::builder::storage::StorageMetadataBuilder;
use crate::builder::{BuildError, ServerBuilder};
use crate::canonicalization::CanonicalizationConfig;
use crate::config::execution::ExecutionConfig;
use crate::config::stage::Stage;
use crate::logging::LoggingConfig;
use crate::middleware::{BodyLimitConfig, CorsConfig, RateLimitConfig};
use crate::network::{NetworkType, RpcSettings};
use crate::release_sweep::ReleaseSweepConfig;

/// HTTP port of a server configured with [`ServerBuilder::from_env`].
pub const DEFAULT_HTTP_PORT: u16 = 3000;
/// gRPC port of a server configured with [`ServerBuilder::from_env`].
pub const DEFAULT_GRPC_PORT: u16 = 50051;

fn invalid(what: &str) -> impl FnOnce(String) -> BuildError + '_ {
    move |error| BuildError::from(format!("Invalid {what}: {error}"))
}

impl ServerBuilder {
    /// The server the official `server` binary runs, configured from the `GUARDIAN_*`
    /// environment variables documented in `docs/CONFIGURATION.md`: storage and metadata, the
    /// acknowledgement keys, network and RPC, canonicalization, Guardian execution, the release
    /// sweep and block watch, rate and body limits, CORS, logging, HTTP on port 3000 and gRPC
    /// on port 50051. Policy modules are read from `GUARDIAN_MODULES_CONFIG` when the server is
    /// built.
    ///
    /// A custom build starts here, so it configures exactly as the official binary does, and
    /// adds its own module kinds; any builder method called afterwards overrides what the
    /// environment set. Loading a `.env` file is left to the binary.
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use server::builder::{BuildError, ServerBuilder};
    /// # fn factory() -> Arc<dyn guardian_module_sdk::ModuleFactory> { unimplemented!() }
    ///
    /// # async fn example() -> Result<(), BuildError> {
    /// ServerBuilder::from_env()
    ///     .await?
    ///     .with_module_kind("acme-sanctions", factory())
    ///     .build()
    ///     .await?
    ///     .run()
    ///     .await;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    /// Returns the first setting that is invalid, or a storage backend or acknowledgement key
    /// store that cannot be opened.
    pub async fn from_env() -> Result<Self, BuildError> {
        let keystore_path: PathBuf = std::env::var("GUARDIAN_KEYSTORE_PATH")
            .unwrap_or_else(|_| "/var/guardian/keystore".to_string())
            .into();

        let (storage, metadata, auditor, coordination) = StorageMetadataBuilder::from_env()
            .build()
            .await
            .map_err(|error| format!("Failed to initialize storage backends: {error}"))?;

        let ack = AckRegistry::new(keystore_path)
            .await
            .map_err(|error| format!("Failed to initialize ack registry: {error}"))?;

        let cors = CorsConfig::from_env()
            .map_err(|error| format!("Failed to initialize CORS config: {error}"))?;
        let network = NetworkType::from_env("GUARDIAN_NETWORK_TYPE")
            .map_err(|error| format!("Failed to resolve network type: {error}"))?;
        let stage = Stage::from_env()
            .map_err(|error| error.to_string())
            .map_err(invalid("GUARDIAN_ENV"))?;
        let rpc = RpcSettings::from_env(network).map_err(invalid("RPC configuration"))?;
        let canonicalization = CanonicalizationConfig::new(10, 48)
            .with_stage_defaults(stage)
            .with_submission_grace_period_seconds(600)
            .with_fast_promotion_enabled_from_env()
            .map_err(invalid("fast promotion configuration"))?
            .with_max_concurrent_accounts_from_env()
            .map_err(invalid("canonicalization concurrency configuration"))?
            .with_retained_ttl_seconds_from_env()
            .map_err(invalid("retained TTL configuration"))?
            .with_reconcile_interval_seconds_from_env()
            .map_err(invalid("reconcile interval configuration"))?
            .with_max_pending_candidates_per_account_from_env()
            .map_err(invalid("pending-candidate queue depth configuration"))?
            .with_rebuild_admission_from_env()
            .map_err(invalid("canonicalization rebuild admission configuration"))?;
        let execution =
            ExecutionConfig::from_env().map_err(invalid("Guardian execution configuration"))?;
        let release_sweep =
            ReleaseSweepConfig::from_env().map_err(invalid("release sweep configuration"))?;
        let block_watch = BlockWatchConfig::from_env_following(release_sweep.enabled)
            .map_err(invalid("block watch configuration"))?;

        Ok(Self::new()
            .with_logging(LoggingConfig::default())
            .network(network)
            .with_rpc(rpc)
            .with_canonicalization(Some(canonicalization))
            .with_execution(execution)
            .with_release_sweep(Some(release_sweep))
            .with_block_watch(Some(block_watch))
            .with_rate_limit(RateLimitConfig::from_env())
            .with_body_limit(BodyLimitConfig::from_env())
            .storage(storage)
            .metadata(metadata)
            .auditor(auditor)
            .coordination(coordination)
            .ack(ack)
            .http(true, DEFAULT_HTTP_PORT)
            .grpc(true, DEFAULT_GRPC_PORT)
            .cors(cors.layer()))
    }
}
