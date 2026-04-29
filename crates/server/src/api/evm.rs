use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::dashboard::extract_cookie;
use crate::error::{GuardianError, Result};
use crate::evm::{EvmProposal, ExecutableEvmProposal};
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ChallengeQuery {
    pub address: String,
}

#[derive(Debug, Serialize)]
pub struct ChallengeResponse {
    pub address: String,
    pub nonce: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub typed_data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct VerifySessionRequest {
    pub address: String,
    pub nonce: String,
    pub signature: String,
}

#[derive(Debug, Serialize)]
pub struct VerifySessionResponse {
    pub address: String,
    pub expires_at: i64,
}

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct RegisterAccountRequest {
    pub chain_id: u64,
    pub account_address: String,
    pub multisig_validator_address: String,
}

#[derive(Debug, Serialize)]
pub struct RegisterAccountResponse {
    pub account_id: String,
    pub chain_id: u64,
    pub account_address: String,
    pub multisig_validator_address: String,
    pub signers: Vec<String>,
    pub threshold: usize,
}

#[derive(Debug, Deserialize)]
pub struct AccountQuery {
    pub account_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateProposalRequest {
    pub account_id: String,
    pub user_op_hash: String,
    pub payload: String,
    pub nonce: String,
    pub ttl_seconds: u64,
    pub signature: String,
}

#[derive(Debug, Serialize)]
pub struct ListProposalsResponse {
    pub proposals: Vec<EvmProposal>,
}

#[derive(Debug, Deserialize)]
pub struct ApproveProposalRequest {
    pub account_id: String,
    pub signature: String,
}

#[derive(Debug, Deserialize)]
pub struct CancelProposalRequest {
    pub account_id: String,
}

#[derive(Debug, Serialize)]
pub struct CancelProposalResponse {
    pub success: bool,
}

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
