//! Server builder for configuring and running the Private State Manager server
//!
//! Provides a fluent API for configuring the server with different:
//! - Network types (Miden, Ethereum, etc.)
//! - Storage backends (Filesystem, S3, PostgreSQL, etc.)
//! - Authentication methods (MidenFalconRpo, EthereumECDSA, etc.)
//! - API protocols (HTTP, gRPC)

use crate::network::NetworkType;
use crate::state::AppState;
use crate::storage::{MetadataStore, StorageRegistry};
use std::sync::Arc;

/// Builder for configuring and creating a server instance
pub struct ServerBuilder {
    network_type: Option<NetworkType>,
    storage: Option<StorageRegistry>,
    metadata: Option<Arc<dyn MetadataStore>>,
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
    ///     .network(NetworkType::Miden);
    /// ```
    pub fn network(mut self, network_type: NetworkType) -> Self {
        self.network_type = Some(network_type);
        self
    }

    /// Set the storage registry
    ///
    /// The storage registry maps storage types to their backend implementations.
    /// Accounts can use different storage backends based on their configuration.
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::storage::StorageRegistry;
    /// use std::path::PathBuf;
    ///
    /// # async fn example() -> Result<(), String> {
    /// // Simple case: use filesystem only
    /// let storage_registry = StorageRegistry::with_filesystem(
    ///     PathBuf::from("/var/psm/storage")
    /// ).await?;
    ///
    /// let builder = ServerBuilder::new()
    ///     .storage(storage_registry);
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// For multiple storage backends, use `StorageRegistry::new()` with a HashMap.
    pub fn storage(mut self, storage: StorageRegistry) -> Self {
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
    /// use server::storage::filesystem::FilesystemMetadataStore;
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
    /// use server::storage::filesystem::{FilesystemService, FilesystemMetadataStore};
    /// use server::storage::{StorageBackend, StorageRegistry, StorageType};
    /// use std::collections::HashMap;
    /// use std::sync::Arc;
    /// use std::path::PathBuf;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let storage = FilesystemService::new(PathBuf::from("/var/psm/storage")).await?;
    /// let metadata = FilesystemMetadataStore::new(PathBuf::from("/var/psm/metadata")).await?;
    ///
    /// let mut backends: HashMap<StorageType, Arc<dyn StorageBackend>> = HashMap::new();
    /// backends.insert(StorageType::Filesystem, Arc::new(storage));
    /// let storage_registry = StorageRegistry::new(backends);
    ///
    /// let handle = ServerBuilder::new()
    ///     .network(NetworkType::Miden)
    ///     .storage(storage_registry)
    ///     .metadata(Arc::new(metadata))
    ///     .build()?;
    ///
    /// handle.run().await;
    /// # Ok(())
    /// # }
    /// ```
    pub fn build(self) -> Result<ServerHandle, String> {
        let network_type = self
            .network_type
            .ok_or("Network type not set. Use .network(NetworkType::Miden)")?;

        let storage = self
            .storage
            .ok_or("Storage registry not set. Use .storage(StorageRegistry::new(...))")?;

        let metadata = self
            .metadata
            .ok_or("Metadata store not set. Use .metadata(...)")?;

        let app_state = AppState {
            storage,
            metadata,
            network_type,
        };

        Ok(ServerHandle {
            app_state,
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

/// Handle for a configured server instance
///
/// Provides methods to run the server with the configured settings.
pub struct ServerHandle {
    app_state: AppState,
    http_enabled: bool,
    http_port: u16,
    grpc_enabled: bool,
    grpc_port: u16,
}

impl ServerHandle {
    /// Run the server with the configured settings
    ///
    /// This will start all enabled servers (HTTP and/or gRPC) and run them
    /// concurrently until the process is terminated.
    ///
    /// # Example
    /// ```no_run
    /// use server::builder::ServerBuilder;
    /// use server::network::NetworkType;
    ///
    /// # async fn example() -> Result<(), String> {
    /// let handle = ServerBuilder::new()
    ///     .network(NetworkType::Miden)
    ///     // ... other configuration
    ///     .build()?;
    ///
    /// handle.run().await;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn run(self) {
        use axum::{Router, routing::get, routing::post};
        use tonic::transport::Server;

        use crate::api::grpc::StateManagerService;
        use crate::api::grpc::state_manager::state_manager_server::StateManagerServer;
        use crate::api::http::{configure, get_delta, get_delta_head, get_state, push_delta};

        async fn root() -> &'static str {
            "Hello, World!"
        }

        let mut tasks = Vec::new();

        // Start HTTP server if enabled
        if self.http_enabled {
            let state = self.app_state.clone();
            let port = self.http_port;

            let task = tokio::spawn(async move {
                let app = Router::new()
                    .route("/", get(root))
                    .route("/delta", post(push_delta))
                    .route("/delta", get(get_delta))
                    .route("/head", get(get_delta_head))
                    .route("/configure", post(configure))
                    .route("/state", get(get_state))
                    .with_state(state);

                let addr = format!("0.0.0.0:{port}");
                let listener = tokio::net::TcpListener::bind(&addr)
                    .await
                    .expect("Failed to bind HTTP server");

                println!(
                    "HTTP server listening on {}",
                    listener.local_addr().unwrap()
                );

                axum::serve(listener, app)
                    .await
                    .expect("HTTP server failed");
            });

            tasks.push(task);
        }

        // Start gRPC server if enabled
        if self.grpc_enabled {
            let state = self.app_state.clone();
            let port = self.grpc_port;

            let task = tokio::spawn(async move {
                let addr = format!("0.0.0.0:{port}")
                    .parse()
                    .expect("Invalid gRPC address");

                let service = StateManagerService { app_state: state };

                // Enable gRPC reflection
                let reflection_service = tonic_reflection::server::Builder::configure()
                    .register_encoded_file_descriptor_set(
                        crate::api::grpc::state_manager::FILE_DESCRIPTOR_SET,
                    )
                    .build_v1()
                    .expect("Failed to build reflection service");

                println!("gRPC server listening on {addr}");

                Server::builder()
                    .add_service(StateManagerServer::new(service))
                    .add_service(reflection_service)
                    .serve(addr)
                    .await
                    .expect("gRPC server failed");
            });

            tasks.push(task);
        }

        if tasks.is_empty() {
            eprintln!("Warning: No servers enabled!");
            return;
        }

        // Wait for all servers
        for task in tasks {
            let _ = task.await;
        }
    }
}
