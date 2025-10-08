use crate::metadata::AccountMetadata;
use crate::state::AppState;
use crate::storage::AccountState;
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct ConfigureRequest {
    pub account_id: String,
    pub initial_state: serde_json::Value,
    pub storage_type: String, // "local" or "S3"
    #[serde(default)]
    pub cosigner_pubkeys: Vec<String>,
}

pub async fn configure(
    State(state): State<AppState>,
    Json(payload): Json<ConfigureRequest>,
) -> StatusCode {
    let now = chrono::Utc::now().to_rfc3339();

    // Create account metadata
    let metadata = AccountMetadata::from(&payload);

    // Store metadata
    if let Err(e) = state.metadata.lock().await.set_account(metadata).await {
        eprintln!("Failed to store account metadata: {}", e);
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    // Create initial account state
    let account_state = AccountState {
        account_id: payload.account_id.clone(),
        state_json: payload.initial_state,
        commitment: String::new(),
        created_at: now.clone(),
        updated_at: now,
    };

    match state.storage.submit_state(&account_state).await {
        Ok(_) => StatusCode::OK,
        Err(e) => {
            eprintln!("Failed to configure account: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
