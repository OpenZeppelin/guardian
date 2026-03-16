//! Server builder for configuring and running the Private State Manager server
//!
//! Provides a fluent API for configuring the server with different:
//! - Network types (Miden, Ethereum, etc.)
//! - Storage backends (Filesystem, S3, PostgreSQL, etc.)
//! - Authentication methods (MidenFalconRpo, EthereumECDSA, etc.)
//! - API protocols (HTTP, gRPC)

pub mod canonicalization;
pub mod clock;
pub mod handle;
pub mod logging;
pub mod state;
pub mod storage;

use crate::ack::AckRegistry;
use crate::builder::handle::ServerHandle;
use crate::canonicalization::CanonicalizationConfig;
use crate::clock::SystemClock;
use crate::logging::LoggingConfig;
use crate::metadata::MetadataStore;
use crate::middleware::{BodyLimitConfig, RateLimitConfig};
use crate::network::{NetworkType, miden::MidenNetworkClient};
use crate::state::AppState;
use crate::storage::StorageBackend;
use private_state_manager_shared::SignatureScheme;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Builder for configuring and creating a server instance
pub struct ServerBuilder {
    network_type: Option<NetworkType>,
    storage: Option<Arc<dyn StorageBackend>>,
    metadata: Option<Arc<dyn MetadataStore>>,
    ack: Option<AckRegistry>,
    canonicalization: Option<CanonicalizationConfig>,
    logging_config: Option<LoggingConfig>,
    cors_layer: Option<tower_http::cors::CorsLayer>,
    rate_limit_config: Option<RateLimitConfig>,
    body_limit_config: Option<BodyLimitConfig>,
    http_enabled: bool,
    http_port: u16,
    grpc_enabled: bool,
    grpc_port: u16,
}

impl ServerBuilder {
    /// Create a new ServerBuilder with default settings
    pub fn new() -> Self {
        Self {
            network_type: None,
            storage: None,
            metadata: None,
            ack: None,
            canonicalization: Some(CanonicalizationConfig::default()),
            logging_config: None,
            cors_layer: None,
            rate_limit_config: None,
            body_limit_config: None,
            http_enabled: true,
            http_port: 3000,
            grpc_enabled: true,
            grpc_port: 50051,
        }
    }

    /// Set the network type (e.g., Miden, Ethereum)
    ///
    /// This determines how account IDs and data structures are validated.
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::network::NetworkType;
    ///
    /// let builder = ServerBuilder::new()
    ///     .network(NetworkType::MidenTestnet);
    /// ```
    pub fn network(mut self, network_type: NetworkType) -> Self {
        self.network_type = Some(network_type);
        self
    }

    /// Set the storage backend
    ///
    /// The server uses a storage backend for accounts.
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::storage::filesystem::FilesystemService;
    /// use std::path::PathBuf;
    /// use std::sync::Arc;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let storage = FilesystemService::new(PathBuf::from("/var/psm/storage")).await?;
    ///
    /// let builder = ServerBuilder::new()
    ///     .storage(Arc::new(storage));
    /// # Ok(())
    /// # }
    /// ```
    pub fn storage(mut self, storage: Arc<dyn StorageBackend>) -> Self {
        self.storage = Some(storage);
        self
    }

    /// Set the metadata store
    ///
    /// Metadata stores handle account configuration and authorization info.
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::metadata::filesystem::FilesystemMetadataStore;
    /// use std::sync::Arc;
    /// use std::path::PathBuf;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let metadata_path = PathBuf::from("/var/psm/metadata");
    /// let metadata = FilesystemMetadataStore::new(metadata_path).await?;
    ///
    /// let builder = ServerBuilder::new()
    ///     .metadata(Arc::new(metadata));
    /// # Ok(())
    /// # }
    /// ```
    pub fn metadata(mut self, metadata: Arc<dyn MetadataStore>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Configure the ack registry for server operations
    ///
    /// The ack registry holds both Falcon and ECDSA signers. The correct signer
    /// is selected per-account based on the account's auth scheme.
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::ack::AckRegistry;
    /// use std::path::PathBuf;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// # let ack = AckRegistry::new(PathBuf::from("/var/psm/keystore"))?;
    ///
    /// # let builder = ServerBuilder::new()
    /// #     .ack(ack);
    /// # Ok(())
    /// # }
    /// ```
    pub fn ack(mut self, ack: AckRegistry) -> Self {
        self.ack = Some(ack);
        self
    }

    /// Configure canonicalization mode
    ///
    /// # Arguments
    /// * `config` - The canonicalization config to use (None for optimistic mode)
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::canonicalization::CanonicalizationConfig;
    ///
    /// // Candidate mode with custom timing
    /// let config = CanonicalizationConfig::new(
    ///     10 * 60,  // 10 minute delay
    ///     30,       // 30 second check interval
    /// );
    /// let builder = ServerBuilder::new()
    ///     .with_canonicalization(Some(config));
    ///
    /// // Optimistic mode - no verification
    /// let builder = ServerBuilder::new()
    ///     .with_canonicalization(None);
    /// ```
    pub fn with_canonicalization(mut self, config: Option<CanonicalizationConfig>) -> Self {
        self.canonicalization = config;
        self
    }

    /// Configure logging
    ///
    /// # Arguments
    /// * `config` - The logging configuration to use
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::logging::LoggingConfig;
    /// use tracing::Level;
    ///
    /// // Default logging (info level with env filter)
    /// let builder = ServerBuilder::new()
    ///     .with_logging(LoggingConfig::default());
    ///
    /// // Custom log level
    /// let builder = ServerBuilder::new()
    ///     .with_logging(LoggingConfig::new(Level::DEBUG));
    ///
    /// // Disable env filter override
    /// let builder = ServerBuilder::new()
    ///     .with_logging(
    ///         LoggingConfig::new(Level::INFO)
    ///             .with_env_filter(false)
    ///     );
    /// ```
    pub fn with_logging(mut self, config: LoggingConfig) -> Self {
        self.logging_config = Some(config);
        self
    }

    /// Configure HTTP server
    ///
    /// # Arguments
    /// * `enabled` - Whether to enable the HTTP server
    /// * `port` - Port number for the HTTP server
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    ///
    /// let builder = ServerBuilder::new()
    ///     .http(true, 8080);
    /// ```
    pub fn http(mut self, enabled: bool, port: u16) -> Self {
        self.http_enabled = enabled;
        self.http_port = port;
        self
    }

    /// Configure gRPC server
    ///
    /// # Arguments
    /// * `enabled` - Whether to enable the gRPC server
    /// * `port` - Port number for the gRPC server
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    ///
    /// let builder = ServerBuilder::new()
    ///     .grpc(true, 50051);
    /// ```
    pub fn grpc(mut self, enabled: bool, port: u16) -> Self {
        self.grpc_enabled = enabled;
        self.grpc_port = port;
        self
    }

    /// Configure CORS for HTTP server
    ///
    /// # Arguments
    /// * `cors_layer` - The CORS layer to use for HTTP requests
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use tower_http::cors::{CorsLayer, Any};
    ///
    /// // Allow all origins (useful for development)
    /// let cors = CorsLayer::new()
    ///     .allow_origin(Any)
    ///     .allow_methods(Any)
    ///     .allow_headers(Any);
    ///
    /// let builder = ServerBuilder::new()
    ///     .cors(cors);
    /// ```
    pub fn cors(mut self, cors_layer: tower_http::cors::CorsLayer) -> Self {
        self.cors_layer = Some(cors_layer);
        self
    }

    /// Configure rate limiting for HTTP server
    ///
    /// Rate limiting uses two windows: burst (per second) and sustained (per minute).
    /// Limits are applied per IP, with optional enhancement based on account/signer.
    ///
    /// # Arguments
    /// * `config` - The rate limit configuration to use
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::middleware::RateLimitConfig;
    ///
    /// // Custom limits
    /// let builder = ServerBuilder::new()
    ///     .with_rate_limit(RateLimitConfig::new(10, 60));
    ///
    /// // Load from environment (PSM_RATE_BURST_PER_SEC, PSM_RATE_PER_MIN)
    /// let builder = ServerBuilder::new()
    ///     .with_rate_limit(RateLimitConfig::from_env());
    ///
    /// ```
    pub fn with_rate_limit(mut self, config: RateLimitConfig) -> Self {
        self.rate_limit_config = Some(config);
        self
    }

    /// Configure maximum request body size for HTTP server
    ///
    /// Limits the size of incoming request bodies to prevent memory exhaustion.
    /// Requests exceeding the limit receive a 413 Payload Too Large response.
    ///
    /// # Arguments
    /// * `config` - The body limit configuration to use
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::middleware::BodyLimitConfig;
    ///
    /// // Custom limit (5 MB)
    /// let builder = ServerBuilder::new()
    ///     .with_body_limit(BodyLimitConfig::new(5 * 1024 * 1024));
    ///
    /// // Load from environment (PSM_MAX_REQUEST_BYTES)
    /// let builder = ServerBuilder::new()
    ///     .with_body_limit(BodyLimitConfig::from_env());
    /// ```
    pub fn with_body_limit(mut self, config: BodyLimitConfig) -> Self {
        self.body_limit_config = Some(config);
        self
    }

    /// Build the server handle
    ///
    /// Validates that all required components are configured and returns
    /// a ServerHandle that can be used to run the server.
    ///
    /// # Errors
    /// Returns an error if any required component is missing.
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::network::NetworkType;
    /// use server::storage::filesystem::FilesystemService;
    /// use server::metadata::filesystem::FilesystemMetadataStore;
    /// use server::storage::StorageBackend;
    /// use std::sync::Arc;
    /// use std::path::PathBuf;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let storage = FilesystemService::new(PathBuf::from("/var/psm/storage")).await?;
    /// let metadata = FilesystemMetadataStore::new(PathBuf::from("/var/psm/metadata")).await?;
    ///
    /// let handle = ServerBuilder::new()
    ///     .network(NetworkType::MidenTestnet)
    ///     .storage(Arc::new(storage))
    ///     .metadata(Arc::new(metadata))
    ///     .build()
    ///     .await?;
    ///
    /// handle.run().await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn build(self) -> Result<ServerHandle, String> {
        if let Some(ref config) = self.logging_config {
            config.init();
        }
        let network_type = self
            .network_type
            .ok_or("Network type not set. Use .network(NetworkType::Miden)")?;

        let storage = self
            .storage
            .ok_or("Storage backend not set. Use .storage(Arc::new(...))")?;

        let metadata = self
            .metadata
            .ok_or("Metadata store not set. Use .metadata(...)")?;

        let ack = self.ack.ok_or("AckRegistry not set. Use .ack(...)")?;

        let network_client = MidenNetworkClient::from_network(network_type)
            .await
            .map_err(|e| format!("Failed to create network client: {e}"))?;

        tracing::info!(
            falcon_commitment = %ack.commitment(&SignatureScheme::Falcon),
            ecdsa_commitment = %ack.commitment(&SignatureScheme::Ecdsa),
            "Server acknowledgement keys initialized"
        );

        let app_state = AppState {
            storage,
            metadata,
            network_client: Arc::new(Mutex::new(network_client)),
            ack,
            canonicalization: self.canonicalization,
            clock: Arc::new(SystemClock),
        };

        Ok(ServerHandle {
            app_state,
            cors_layer: self.cors_layer,
            rate_limit_config: self.rate_limit_config,
            body_limit_config: self.body_limit_config,
            http_enabled: self.http_enabled,
            http_port: self.http_port,
            grpc_enabled: self.grpc_enabled,
            grpc_port: self.grpc_port,
        })
    }
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ServerHandle moved to builder::handle
