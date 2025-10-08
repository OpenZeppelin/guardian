pub use private_state_manager_shared::{FromJson, ToJson};

use axum::{routing::get, routing::post, Router};

pub mod config;
pub mod handlers;
pub mod metadata;
pub mod state;
pub mod storage;

use config::{initialize_metadata, initialize_storage};
use handlers::{configure, get_delta, get_delta_head, get_state, push_delta};
use state::AppState;

async fn root() -> &'static str {
    "Hello, World!"
}

/// Main server entrypoint.
pub async fn run() {
    let metadata = initialize_metadata()
        .await
        .expect("Failed to initialize metadata");

    let storage = initialize_storage()
        .await
        .expect("Failed to initialize storage");

    let app_state = AppState { storage, metadata };

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
    println!("Listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
