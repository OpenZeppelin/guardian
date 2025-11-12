use crate::delta_object::DeltaObject;
use crate::metadata::auth::{Auth, AuthHeader, Credentials};
use crate::services::{
    self, ConfigureAccountParams, GetDeltaParams, GetDeltaProposalsParams, GetDeltaSinceParams,
    GetStateParams, PushDeltaParams, PushDeltaProposalParams, SignDeltaProposalParams,
};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::storage::StorageType;
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
            // Credential will be set from AuthHeader
            credential: Credentials::signature(String::new(), String::new()),
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

#[derive(Deserialize)]
pub struct ProposalQuery {
    pub account_id: String,
}

#[derive(Deserialize)]
pub struct DeltaProposalRequest {
    pub account_id: String,
    pub nonce: u64,
    pub delta_payload: serde_json::Value,
}

#[derive(Deserialize)]
pub struct SignProposalRequest {
    pub account_id: String,
    pub commitment: String,
    pub signature_scheme: String,
    pub signature: String,
}

// Response types
#[derive(Serialize)]
pub struct ConfigureResponse {
    pub success: bool,
    pub message: String,
    pub ack_pubkey: Option<String>,
}

#[derive(Serialize)]
pub struct ErrorResponse {
    pub success: bool,
    pub error: String,
}

pub async fn configure(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<ConfigureRequest>,
) -> (StatusCode, Json<ConfigureResponse>) {
    let mut params = ConfigureAccountParams::from(payload);
    params.credential = credentials;

    match services::configure_account(&state, params).await {
        Ok(response) => (
            StatusCode::OK,
            Json(ConfigureResponse {
                success: true,
                message: format!("Account '{}' configured successfully", response.account_id),
                ack_pubkey: Some(response.ack_pubkey),
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(ConfigureResponse {
                success: false,
                message: e.to_string(),
                ack_pubkey: None,
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
) -> (StatusCode, Json<StateObject>) {
    let params = GetStateParams {
        account_id: query.account_id,
        credentials,
    };

    match services::get_state(&state, params).await {
        Ok(response) => (StatusCode::OK, Json(response.state)),
        Err(e) => (
            StatusCode::NOT_FOUND,
            Json(StateObject {
                account_id: e.to_string(),
                ..Default::default()
            }),
        ),
    }
}

#[derive(Serialize)]
pub struct PubkeyResponse {
    pub pubkey: String,
}

#[derive(Serialize)]
pub struct ProposalsResponse {
    pub proposals: Vec<DeltaObject>,
}

#[derive(Serialize)]
pub struct DeltaProposalResponse {
    pub delta: DeltaObject,
    pub commitment: String,
}

pub async fn get_pubkey(State(state): State<AppState>) -> (StatusCode, Json<PubkeyResponse>) {
    let pubkey = state.ack.commitment();
    (StatusCode::OK, Json(PubkeyResponse { pubkey }))
}

pub async fn push_delta_proposal(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<DeltaProposalRequest>,
) -> (StatusCode, Json<DeltaProposalResponse>) {
    let params = PushDeltaProposalParams {
        account_id: payload.account_id,
        nonce: payload.nonce,
        delta_payload: payload.delta_payload,
        credentials,
    };

    match services::push_delta_proposal(&state, params).await {
        Ok(response) => (
            StatusCode::OK,
            Json(DeltaProposalResponse {
                delta: response.delta,
                commitment: response.commitment,
            }),
        ),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(DeltaProposalResponse {
                delta: DeltaObject {
                    account_id: e.to_string(),
                    ..Default::default()
                },
                commitment: String::new(),
            }),
        ),
    }
}

pub async fn get_delta_proposals(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Query(query): Query<ProposalQuery>,
) -> (StatusCode, Json<ProposalsResponse>) {
    let params = GetDeltaProposalsParams {
        account_id: query.account_id,
        credentials,
    };

    match services::get_delta_proposals(&state, params).await {
        Ok(response) => (
            StatusCode::OK,
            Json(ProposalsResponse {
                proposals: response.proposals,
            }),
        ),
        Err(_e) => (
            StatusCode::OK,
            Json(ProposalsResponse {
                proposals: Vec::new(),
            }),
        ),
    }
}

pub async fn sign_delta_proposal(
    State(state): State<AppState>,
    AuthHeader(credentials): AuthHeader,
    Json(payload): Json<SignProposalRequest>,
) -> (StatusCode, Json<DeltaObject>) {
    let params = SignDeltaProposalParams {
        account_id: payload.account_id,
        commitment: payload.commitment,
        signature_scheme: payload.signature_scheme,
        signature: payload.signature,
        credentials,
    };

    match services::sign_delta_proposal(&state, params).await {
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
