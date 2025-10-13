pub use private_state_manager_shared::{FromJson, ToJson};

use axum::{routing::get, routing::post, Router};
use tonic::transport::Server;

pub mod config;
pub mod grpc;
pub mod http;
pub mod metadata;
pub mod services;
pub mod state;
pub mod storage;

use config::{initialize_metadata, initialize_storage};
use grpc::state_manager::state_manager_server::StateManagerServer;
use grpc::StateManagerService;
use http::{configure, get_delta, get_delta_head, get_state, push_delta};
use state::AppState;

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

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .unwrap();
    println!("HTTP server listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

/// Run gRPC server
async fn run_grpc_server(app_state: AppState) {
    let addr = "0.0.0.0:50051".parse().unwrap();
    let service = StateManagerService { app_state };

    // Enable gRPC reflection
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(grpc::state_manager::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();

    println!("gRPC server listening on {}", addr);

    Server::builder()
        .add_service(StateManagerServer::new(service))
        .add_service(reflection_service)
        .serve(addr)
        .await
        .unwrap();
}

/// Main server entrypoint - runs both HTTP and gRPC servers
pub async fn run() {
    let metadata = initialize_metadata()
        .await
        .expect("Failed to initialize metadata");

    let storage = initialize_storage()
        .await
        .expect("Failed to initialize storage");

    let app_state = AppState { storage, metadata };

    let grpc_app_state = AppState {
        storage: app_state.storage.clone(),
        metadata: app_state.metadata.clone(),
    };

    // Run both servers concurrently
    tokio::join!(
        run_http_server(app_state),
        run_grpc_server(grpc_app_state)
    );
}
