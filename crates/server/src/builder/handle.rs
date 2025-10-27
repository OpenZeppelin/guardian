use axum::{Router, routing::get, routing::post};
use tonic::transport::Server;
use tower_http::cors::CorsLayer;

use crate::api::grpc::StateManagerService;
use crate::api::grpc::state_manager::state_manager_server::StateManagerServer;
use crate::api::http::{configure, get_delta, get_delta_since, get_state, push_delta};
use crate::state::AppState;

/// Handle for a configured server instance
///
/// Provides methods to run the server with the configured settings.
pub struct ServerHandle {
    pub(crate) app_state: AppState,
    pub(crate) cors_layer: Option<CorsLayer>,
    pub(crate) http_enabled: bool,
    pub(crate) http_port: u16,
    pub(crate) grpc_enabled: bool,
    pub(crate) grpc_port: u16,
}

impl ServerHandle {
    /// Run the server with the configured settings
    pub async fn run(self) {
        async fn root() -> &'static str {
            "Hello, World!"
        }

        let mut tasks = Vec::new();

        // Start background jobs based on canonicalization config
        if let Some(config) = &self.app_state.canonicalization {
            tracing::info!(
                delay_seconds = config.delay_seconds,
                check_interval_seconds = config.check_interval_seconds,
                "Starting canonicalization worker"
            );
            crate::services::start_canonicalization_worker(self.app_state.clone());
        } else {
            tracing::info!(
                "Running in optimistic mode - deltas accepted without on-chain verification"
            );
        }

        // Start HTTP server if enabled
        if self.http_enabled {
            let state = self.app_state.clone();
            let port = self.http_port;
            let cors_layer = self.cors_layer.clone();

            let task = tokio::spawn(async move {
                let mut app = Router::new()
                    .route("/", get(root))
                    .route("/delta", post(push_delta))
                    .route("/delta", get(get_delta))
                    .route("/delta/since", get(get_delta_since))
                    .route("/configure", post(configure))
                    .route("/state", get(get_state))
                    .with_state(state);

                if let Some(cors) = cors_layer {
                    app = app.layer(cors);
                }

                let addr = format!("0.0.0.0:{port}");
                let listener = tokio::net::TcpListener::bind(&addr)
                    .await
                    .expect("Failed to bind HTTP server");

                tracing::info!(
                    address = %listener.local_addr().unwrap(),
                    "HTTP server listening"
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

                tracing::info!(address = %addr, "gRPC server listening");

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
            tracing::warn!("No servers enabled");
            return;
        }

        // Wait for all servers
        for task in tasks {
            let _ = task.await;
        }
    }
}
