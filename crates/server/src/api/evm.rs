use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::dashboard::extract_cookie;
use crate::error::{GuardianError, Result};
use crate::evm::{EvmProposal, ExecutableEvmProposal};
use crate::state::AppState;

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ChallengeQuery {
    pub address: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ChallengeResponse {
    pub address: String,
    pub nonce: String,
    pub issued_at: i64,
    pub expires_at: i64,
    /// EIP-712 typed-data payload the wallet signs to establish a session.
    #[schema(value_type = Object)]
    pub typed_data: serde_json::Value,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct VerifySessionRequest {
    pub address: String,
    pub nonce: String,
    pub signature: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct VerifySessionResponse {
    pub address: String,
    pub expires_at: i64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct LogoutResponse {
    pub success: bool,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RegisterAccountRequest {
    pub chain_id: u64,
    pub account_address: String,
    pub multisig_validator_address: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RegisterAccountResponse {
    pub account_id: String,
    pub chain_id: u64,
    pub account_address: String,
    pub multisig_validator_address: String,
    pub signers: Vec<String>,
    pub threshold: usize,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AccountQuery {
    pub account_id: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateProposalRequest {
    pub account_id: String,
    pub user_op_hash: String,
    pub payload: String,
    pub nonce: String,
    pub ttl_seconds: u64,
    pub signature: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ListProposalsResponse {
    pub proposals: Vec<EvmProposal>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ApproveProposalRequest {
    pub account_id: String,
    pub signature: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CancelProposalRequest {
    pub account_id: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CancelProposalResponse {
    pub success: bool,
}

/// Issue an EIP-712 session challenge for an EVM wallet address.
#[utoipa::path(
    get,
    path = "/evm/auth/challenge",
    tag = "evm",
    params(ChallengeQuery),
    responses(
        (status = 200, description = "Challenge issued", body = ChallengeResponse),
        (status = 400, description = "Invalid address", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn challenge_evm_session(
    State(state): State<AppState>,
    Query(query): Query<ChallengeQuery>,
) -> Result<Json<ChallengeResponse>> {
    let challenge = state
        .evm
        .sessions
        .issue_challenge(&query.address, state.clock.now())
        .await?;
    Ok(Json(ChallengeResponse {
        address: challenge.address.clone(),
        nonce: challenge.nonce.clone(),
        issued_at: challenge.issued_at.timestamp(),
        expires_at: challenge.expires_at.timestamp(),
        typed_data: session_typed_data(&challenge),
    }))
}

/// Verify a signed EVM session challenge and establish a session
/// (sets a session cookie on success).
#[utoipa::path(
    post,
    path = "/evm/auth/verify",
    tag = "evm",
    request_body = VerifySessionRequest,
    responses(
        (status = 200, description = "Session established", body = VerifySessionResponse),
        (status = 401, description = "Challenge verification failed", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn verify_evm_session(
    State(state): State<AppState>,
    Json(request): Json<VerifySessionRequest>,
) -> Result<(
    [(header::HeaderName, String); 1],
    Json<VerifySessionResponse>,
)> {
    let session = state
        .evm
        .sessions
        .verify(
            &request.address,
            &request.nonce,
            &request.signature,
            state.clock.now(),
        )
        .await?;
    Ok((
        [(header::SET_COOKIE, session.cookie_header)],
        Json(VerifySessionResponse {
            address: session.address,
            expires_at: session.expires_at.timestamp_millis(),
        }),
    ))
}

/// Invalidate the current EVM session and clear the session cookie.
#[utoipa::path(
    post,
    path = "/evm/auth/logout",
    tag = "evm",
    security(("evm_session" = [])),
    responses(
        (status = 200, description = "Session invalidated", body = LogoutResponse),
    )
)]
pub async fn logout_evm_session(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<([(header::HeaderName, String); 1], Json<LogoutResponse>)> {
    let token = extract_cookie(&headers, state.evm.sessions.cookie_name());
    state
        .evm
        .sessions
        .logout(token.as_deref(), state.clock.now())
        .await;
    Ok((
        [(header::SET_COOKIE, state.evm.sessions.clear_cookie_header())],
        Json(LogoutResponse { success: true }),
    ))
}

/// Register an EVM smart-account with Guardian (requires an EVM session).
#[utoipa::path(
    post,
    path = "/evm/accounts",
    tag = "evm",
    security(("evm_session" = [])),
    request_body = RegisterAccountRequest,
    responses(
        (status = 200, description = "Account registered", body = RegisterAccountResponse),
        (status = 400, description = "Invalid network config", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "Missing EVM session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Session signer not authorized for the account", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn register_evm_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RegisterAccountRequest>,
) -> Result<Json<RegisterAccountResponse>> {
    let session_address = require_evm_session(&state, &headers).await?;
    let response = crate::evm::service::register_account(
        &state,
        crate::evm::service::RegisterEvmAccountParams {
            chain_id: request.chain_id,
            account_address: request.account_address,
            multisig_validator_address: request.multisig_validator_address,
            session_address,
        },
    )
    .await?;
    Ok(Json(RegisterAccountResponse {
        account_id: response.account_id,
        chain_id: response.chain_id,
        account_address: response.account_address,
        multisig_validator_address: response.multisig_validator_address,
        signers: response.signers,
        threshold: response.threshold,
    }))
}

/// Create a new EVM multisig proposal (requires an EVM session).
#[utoipa::path(
    post,
    path = "/evm/proposals",
    tag = "evm",
    security(("evm_session" = [])),
    request_body = CreateProposalRequest,
    responses(
        (status = 200, description = "Proposal created", body = EvmProposal),
        (status = 400, description = "Invalid proposal input", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "Missing EVM session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Session signer not authorized for the account", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn create_evm_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateProposalRequest>,
) -> Result<Json<EvmProposal>> {
    let session_address = require_evm_session(&state, &headers).await?;
    let proposal = crate::evm::service::create_proposal(
        &state,
        crate::evm::service::CreateEvmProposalParams {
            account_id: request.account_id,
            user_op_hash: request.user_op_hash,
            payload: request.payload,
            nonce: request.nonce,
            ttl_seconds: request.ttl_seconds,
            signature: request.signature,
            session_address,
        },
    )
    .await?;
    Ok(Json(proposal))
}

/// List EVM proposals for an account (requires an EVM session).
#[utoipa::path(
    get,
    path = "/evm/proposals",
    tag = "evm",
    security(("evm_session" = [])),
    params(AccountQuery),
    responses(
        (status = 200, description = "Proposals", body = ListProposalsResponse),
        (status = 401, description = "Missing EVM session", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn list_evm_proposals(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AccountQuery>,
) -> Result<Json<ListProposalsResponse>> {
    let session_address = require_evm_session(&state, &headers).await?;
    let proposals =
        crate::evm::service::list_proposals(&state, &query.account_id, &session_address).await?;
    Ok(Json(ListProposalsResponse { proposals }))
}

/// Fetch a single EVM proposal by id (requires an EVM session).
#[utoipa::path(
    get,
    path = "/evm/proposals/{proposal_id}",
    tag = "evm",
    security(("evm_session" = [])),
    params(("proposal_id" = String, Path, description = "Proposal identifier"), AccountQuery),
    responses(
        (status = 200, description = "Proposal", body = EvmProposal),
        (status = 401, description = "Missing EVM session", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Proposal not found", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn get_evm_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(proposal_id): Path<String>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<EvmProposal>> {
    let session_address = require_evm_session(&state, &headers).await?;
    let proposal = crate::evm::service::get_proposal(
        &state,
        &query.account_id,
        &proposal_id,
        &session_address,
    )
    .await?;
    Ok(Json(proposal))
}

/// Add an approval signature to an EVM proposal (requires an EVM session).
#[utoipa::path(
    post,
    path = "/evm/proposals/{proposal_id}/approve",
    tag = "evm",
    security(("evm_session" = [])),
    params(("proposal_id" = String, Path, description = "Proposal identifier")),
    request_body = ApproveProposalRequest,
    responses(
        (status = 200, description = "Approval recorded", body = EvmProposal),
        (status = 400, description = "Invalid signature", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "Missing EVM session", body = crate::openapi::ApiErrorResponse),
        (status = 403, description = "Session signer not authorized for the account", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Proposal not found", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn approve_evm_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(proposal_id): Path<String>,
    Json(request): Json<ApproveProposalRequest>,
) -> Result<Json<EvmProposal>> {
    let session_address = require_evm_session(&state, &headers).await?;
    let proposal = crate::evm::service::approve_proposal(
        &state,
        crate::evm::service::ApproveEvmProposalParams {
            account_id: request.account_id,
            proposal_id,
            signature: request.signature,
            session_address,
        },
    )
    .await?;
    Ok(Json(proposal))
}

/// Fetch the executable (threshold-met) form of an EVM proposal,
/// ready for on-chain submission (requires an EVM session).
#[utoipa::path(
    get,
    path = "/evm/proposals/{proposal_id}/executable",
    tag = "evm",
    security(("evm_session" = [])),
    params(("proposal_id" = String, Path, description = "Proposal identifier"), AccountQuery),
    responses(
        (status = 200, description = "Executable proposal", body = ExecutableEvmProposal),
        (status = 400, description = "Proposal not yet executable", body = crate::openapi::ApiErrorResponse),
        (status = 401, description = "Missing EVM session", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Proposal not found", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn get_executable_evm_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(proposal_id): Path<String>,
    Query(query): Query<AccountQuery>,
) -> Result<Json<ExecutableEvmProposal>> {
    let session_address = require_evm_session(&state, &headers).await?;
    let executable = crate::evm::service::executable_proposal(
        &state,
        &query.account_id,
        &proposal_id,
        &session_address,
    )
    .await?;
    Ok(Json(executable))
}

/// Cancel an EVM proposal (requires an EVM session).
#[utoipa::path(
    post,
    path = "/evm/proposals/{proposal_id}/cancel",
    tag = "evm",
    security(("evm_session" = [])),
    params(("proposal_id" = String, Path, description = "Proposal identifier")),
    request_body = CancelProposalRequest,
    responses(
        (status = 200, description = "Proposal cancelled", body = CancelProposalResponse),
        (status = 401, description = "Missing EVM session", body = crate::openapi::ApiErrorResponse),
        (status = 404, description = "Proposal not found", body = crate::openapi::ApiErrorResponse),
    )
)]
pub async fn cancel_evm_proposal(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(proposal_id): Path<String>,
    Json(request): Json<CancelProposalRequest>,
) -> Result<Json<CancelProposalResponse>> {
    let session_address = require_evm_session(&state, &headers).await?;
    crate::evm::service::cancel_proposal(
        &state,
        &request.account_id,
        &proposal_id,
        &session_address,
    )
    .await?;
    Ok(Json(CancelProposalResponse { success: true }))
}

pub async fn require_evm_session(state: &AppState, headers: &HeaderMap) -> Result<String> {
    let token = extract_cookie(headers, state.evm.sessions.cookie_name())
        .ok_or_else(|| GuardianError::AuthenticationFailed("Missing EVM session".to_string()))?;
    Ok(state
        .evm
        .sessions
        .authenticate(&token, state.clock.now())
        .await?
        .address)
}

fn session_typed_data(challenge: &crate::evm::session::EvmChallenge) -> serde_json::Value {
    json!({
        "domain": {
            "name": "Guardian EVM Session",
            "version": "1"
        },
        "types": {
            "EIP712Domain": [
                { "name": "name", "type": "string" },
                { "name": "version", "type": "string" }
            ],
            "GuardianEvmSession": [
                { "name": "wallet", "type": "address" },
                { "name": "nonce", "type": "bytes32" },
                { "name": "issued_at", "type": "uint64" },
                { "name": "expires_at", "type": "uint64" }
            ]
        },
        "primaryType": "GuardianEvmSession",
        "message": {
            "wallet": challenge.address,
            "nonce": challenge.nonce,
            "issued_at": challenge.issued_at.timestamp(),
            "expires_at": challenge.expires_at.timestamp()
        }
    })
}
