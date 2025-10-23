use crate::metadata::auth::{Auth, AuthHeader};
use crate::services::{
    self, ConfigureAccountParams, GetDeltaParams, GetDeltaSinceParams, GetStateParams,
    PushDeltaParams,
};
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject, StorageType};
use axum::{Json, extract::Query, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ConfigureRequest {
    pub account_id: String,
    pub auth: Auth,
    pub initial_state: serde_json::Value,
    pub storage_type: StorageType,
}

impl From<ConfigureRequest> for ConfigureAccountParams {
    fn from(req: ConfigureRequest) -> Self {
        Self {
            account_id: req.account_id,
            auth: req.auth,
            initial_state: req.initial_state,
            storage_type: req.storage_type,
        }
    }
}

#[derive(Deserialize)]
pub struct DeltaQuery {
    pub account_id: String,
    pub nonce: u64,
}

#[derive(Deserialize)]
pub struct StateQuery {
    pub account_id: String,
}

// Response types
#[derive(Serialize)]
pub struct ConfigureResponse {
    pub success: bool,
    pub message: String,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub success: bool,
    pub error: String,
}

pub async fn configure(
    State(state): State<AppState>,
    Json(payload): Json<ConfigureRequest>,
) -> (StatusCode, Json<ConfigureResponse>) {
    let params = ConfigureAccountParams::from(payload);

    match services::configure_account(&state, params).await {
        Ok(response) => (
            StatusCode::OK,
            Json(ConfigureResponse {
                success: true,
                message: format!("Account '{}' configured successfully", response.account_id),
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ConfigureResponse {
                success: false,
                message: e.to_string(),
            }),
        ),
    }
}

pub async fn push_delta(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<DeltaObject>,
) -> (StatusCode, Json<DeltaObject>) {
    let params = PushDeltaParams {
        delta: payload,
        credentials,
    };

    match services::push_delta(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.delta)),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(DeltaObject {
                account_id: e.to_string(),
                ..Default::default()
            }),
        ),
    }
}

pub async fn get_delta(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<DeltaQuery>,
) -> (StatusCode, Json<DeltaObject>) {
    let params = GetDeltaParams {
        account_id: query.account_id,
        nonce: query.nonce,
        credentials,
    };

    match services::get_delta(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.delta)),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(DeltaObject {
                account_id: e.to_string(),
                ..Default::default()
            }),
        ),
    }
}

pub async fn get_delta_since(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<DeltaQuery>,
) -> (StatusCode, Json<DeltaObject>) {
    let params = GetDeltaSinceParams {
        account_id: query.account_id,
        from_nonce: query.nonce,
        credentials,
    };

    match services::get_delta_since(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.merged_delta)),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(DeltaObject {
                account_id: e.to_string(),
                ..Default::default()
            }),
        ),
    }
}

pub async fn get_state(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<StateQuery>,
) -> (StatusCode, Json<AccountState>) {
    let params = GetStateParams {
        account_id: query.account_id,
        credentials,
    };

    match services::get_state(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.state)),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(AccountState {
                account_id: e.to_string(),
                ..Default::default()
            }),
        ),
    }
}
