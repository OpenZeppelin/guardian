pub use private_state_manager_shared::{FromJson, ToJson};

use axum::{Router, routing::get, routing::post};
use tonic::transport::Server;

pub mod api;
pub mod auth;
pub mod network;
pub mod services;
pub mod state;
pub mod storage;

use api::grpc::StateManagerService;
use api::grpc::state_manager::state_manager_server::StateManagerServer;
use api::http::{configure, get_delta, get_delta_head, get_state, push_delta};
use network::NetworkType;
use state::AppState;
use std::sync::Arc;
use storage::filesystem::{FilesystemConfig, FilesystemMetadataStore, FilesystemService};

async fn root() -> &'static str {
    "Hello, World!"
}

/// Run HTTP server
async fn run_http_server(app_state: AppState) {
    let app = Router::new()
        .route("/", get(root))
        .route("/delta", post(push_delta))
        .route("/delta", get(get_delta))
        .route("/head", get(get_delta_head))
        .route("/configure", post(configure))
        .route("/state", get(get_state))
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!(
        "HTTP server listening on {}",
        listener.local_addr().unwrap()
    );
    axum::serve(listener, app).await.unwrap();
}

/// Run gRPC server
async fn run_grpc_server(app_state: AppState) {
    let addr = "0.0.0.0:50051".parse().unwrap();
    let service = StateManagerService { app_state };

    // Enable gRPC reflection
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(api::grpc::state_manager::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();

    println!("gRPC server listening on {addr}");

    Server::builder()
        .add_service(StateManagerServer::new(service))
        .add_service(reflection_service)
        .serve(addr)
        .await
        .unwrap();
}

/// Main server entrypoint - runs both HTTP and gRPC servers
pub async fn run() {
    // Load configuration from environment
    let config = FilesystemConfig::from_env().expect("Failed to load configuration");

    // Create storage and metadata stores
    let storage = FilesystemService::new(config.clone())
        .await
        .expect("Failed to initialize filesystem storage");

    let metadata = FilesystemMetadataStore::new(config.app_path)
        .await
        .expect("Failed to initialize metadata store");

    // Create app state with Miden network type
    // In the future, this will be configurable via builder pattern
    let app_state = AppState {
        storage: Arc::new(storage),
        metadata: Arc::new(metadata),
        network_type: NetworkType::Miden,
    };

    let grpc_app_state = app_state.clone();

    // Run both servers concurrently
    tokio::join!(run_http_server(app_state), run_grpc_server(grpc_app_state));
}
