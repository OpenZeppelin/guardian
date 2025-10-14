use crate::auth::AuthType;
use crate::services::{self, AuthData, ConfigureAccountParams, PushDeltaParams, GetDeltaParams, GetDeltaHeadParams, GetStateParams};
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject};
use axum::{
    extract::Query,
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct ConfigureRequest {
    pub account_id: String,
    pub auth_type: AuthType,
    pub initial_state: serde_json::Value,
    pub storage_type: String,
    #[serde(default)]
    pub cosigner_pubkeys: Vec<String>,
}

impl From<ConfigureRequest> for ConfigureAccountParams {
    fn from(req: ConfigureRequest) -> Self {
        Self {
            account_id: req.account_id,
            auth_type: req.auth_type,
            initial_state: req.initial_state,
            storage_type: req.storage_type,
            cosigner_pubkeys: req.cosigner_pubkeys,
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

#[derive(Serialize)]
pub struct DeltaHeadResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_nonce: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}


/// Extract authentication data from HTTP headers
fn extract_auth(headers: &HeaderMap) -> Result<AuthData, String> {
    let pubkey = headers
        .get("x-pubkey")
        .ok_or_else(|| "Missing x-pubkey header".to_string())?
        .to_str()
        .map_err(|_| "Invalid x-pubkey header".to_string())?
        .to_string();

    let signature = headers
        .get("x-signature")
        .ok_or_else(|| "Missing x-signature header".to_string())?
        .to_str()
        .map_err(|_| "Invalid x-signature header".to_string())?
        .to_string();

    Ok(AuthData { pubkey, signature })
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
                message: e.message,
            }),
        ),
    }
}

pub async fn push_delta(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DeltaObject>,
) -> (StatusCode, Json<DeltaObject>) {
    // Extract authentication data from headers
    let auth = match extract_auth(&headers) {
        Ok(auth) => auth,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(DeltaObject {
                    account_id: e,
                    ..Default::default()
                }),
            )
        }
    };

    let params = PushDeltaParams {
        delta: payload,
        auth,
    };

    match services::push_delta(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.delta)),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(DeltaObject {
                account_id: e.message,
                ..Default::default()
            }),
        ),
    }
}

pub async fn get_delta(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DeltaQuery>,
) -> (StatusCode, Json<DeltaObject>) {
    // Extract authentication data from headers
    let auth = match extract_auth(&headers) {
        Ok(auth) => auth,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(DeltaObject {
                    account_id: e,
                    ..Default::default()
                }),
            )
        }
    };

    let params = GetDeltaParams {
        account_id: query.account_id,
        nonce: query.nonce,
        auth,
    };

    match services::get_delta(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.delta)),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(DeltaObject {
                account_id: e.message,
                ..Default::default()
            }),
        ),
    }
}

pub async fn get_delta_head(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StateQuery>,
) -> (StatusCode, Json<DeltaHeadResponse>) {
    // Extract authentication data from headers
    let auth = match extract_auth(&headers) {
        Ok(auth) => auth,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(DeltaHeadResponse {
                    success: false,
                    latest_nonce: None,
                    message: Some(e),
                }),
            )
        }
    };

    let params = GetDeltaHeadParams {
        account_id: query.account_id,
        auth,
    };

    match services::get_delta_head(&state, params).await {
        Ok(response) => (
            StatusCode::OK,
            Json(DeltaHeadResponse {
                success: true,
                latest_nonce: Some(response.delta.nonce),
                message: Some("Latest delta retrieved successfully".to_string()),
            }),
        ),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(DeltaHeadResponse {
                success: false,
                latest_nonce: None,
                message: Some(e.message),
            }),
        ),
    }
}

pub async fn get_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StateQuery>,
) -> (StatusCode, Json<AccountState>) {
    // Extract authentication data from headers
    let auth = match extract_auth(&headers) {
        Ok(auth) => auth,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(AccountState {
                    account_id: e,
                    ..Default::default()
                }),
            )
        }
    };

    let params = GetStateParams {
        account_id: query.account_id,
        auth,
    };

    match services::get_state(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.state)),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(AccountState {
                account_id: e.message,
                ..Default::default()
            }),
        ),
    }
}
