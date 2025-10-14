use crate::auth::AuthType;
use crate::metadata::AccountMetadata;
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject};

pub type ServiceResult<T> = Result<T, ServiceError>;

#[derive(Debug, Clone)]
pub struct ServiceError {
    pub message: String,
}

impl ServiceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Authentication data extracted from API requests
#[derive(Debug, Clone)]
pub struct AuthData {
    pub pubkey: String,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct ConfigureAccountParams {
    pub account_id: String,
    pub auth_type: AuthType,
    pub initial_state: serde_json::Value,
    pub storage_type: String,
    pub cosigner_pubkeys: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ConfigureAccountResult {
    pub account_id: String,
}

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub auth: AuthData,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

#[derive(Debug, Clone)]
pub struct GetDeltaParams {
    pub account_id: String,
    pub nonce: u64,
    pub auth: AuthData,
}

#[derive(Debug, Clone)]
pub struct GetDeltaResult {
    pub delta: DeltaObject,
}

#[derive(Debug, Clone)]
pub struct GetDeltaHeadParams {
    pub account_id: String,
    pub auth: AuthData,
}

#[derive(Debug, Clone)]
pub struct GetDeltaHeadResult {
    pub delta: DeltaObject,
}

#[derive(Debug, Clone)]
pub struct GetStateParams {
    pub account_id: String,
    pub auth: AuthData,
}

#[derive(Debug, Clone)]
pub struct GetStateResult {
    pub state: AccountState,
}

/// Verify that the public key is authorized (in the cosigner list)
fn verify_authorized(
    account_metadata: &AccountMetadata,
    pubkey: &str,
) -> ServiceResult<()> {
    if account_metadata.cosigner_pubkeys.contains(&pubkey.to_string()) {
        Ok(())
    } else {
        Err(ServiceError::new(format!(
            "Public key '{}' is not authorized for account '{}'",
            pubkey, account_metadata.account_id
        )))
    }
}

/// Verify signature and authorization for a request
fn verify_request_auth(
    auth_type: &AuthType,
    account_metadata: &AccountMetadata,
    account_id: &str,
    pubkey: &str,
    signature: &str,
) -> ServiceResult<()> {
    // Check if public key is authorized
    verify_authorized(account_metadata, pubkey)?;

    // Verify signature
    auth_type
        .verify_signature(account_id, pubkey, signature)
        .map_err(|e| ServiceError::new(format!("Signature verification failed: {}", e)))
}

/// Configure a new account
pub async fn configure_account(
    state: &AppState,
    params: ConfigureAccountParams,
) -> ServiceResult<ConfigureAccountResult> {
    // Check if account already exists
    let mut metadata = state.metadata.lock().await;
    let existing = metadata.get_account(&params.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check existing account: {}", e)))?;

    if existing.is_some() {
        return Err(ServiceError::new(format!("Account '{}' already exists", params.account_id)));
    }

    // Create initial account state
    let now = chrono::Utc::now().to_rfc3339();
    let account_state = AccountState {
        account_id: params.account_id.clone(),
        state_json: params.initial_state,
        commitment: String::new(), // TODO: calculate commitment + validate vs on-chain commitment.
        created_at: now.clone(),
        updated_at: now,
    };

    // Submit initial state to storage
    state.storage.submit_state(&account_state).await
        .map_err(|e| ServiceError::new(format!("Failed to submit initial state: {}", e)))?;

    // Create and store metadata
    let metadata_entry = AccountMetadata {
        account_id: params.account_id.clone(),
        auth_type: params.auth_type,
        storage_type: params.storage_type,
        cosigner_pubkeys: params.cosigner_pubkeys,
        created_at: account_state.created_at.clone(),
        updated_at: account_state.updated_at.clone(),
    };

    metadata.set_account(metadata_entry).await
        .map_err(|e| ServiceError::new(format!("Failed to store metadata: {}", e)))?;

    Ok(ConfigureAccountResult {
        account_id: params.account_id,
    })
}

/// Push a delta
pub async fn push_delta(
    state: &AppState,
    params: PushDeltaParams,
) -> ServiceResult<PushDeltaResult> {
    // Verify account exists
    let metadata = state.metadata.lock().await;
    let account_metadata = metadata.get_account(&params.delta.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {}", e)))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", params.delta.account_id)))?;
    drop(metadata);

    // Verify signature and authorization
    verify_request_auth(
        &account_metadata.auth_type,
        &account_metadata,
        &params.delta.account_id,
        &params.auth.pubkey,
        &params.auth.signature,
    )?;

    // TODO: Verify prev_commitment matches current state commitment
    // TODO: Verify new commitment vs on-chain commitment in time window.

    // Submit delta to storage
    state.storage.submit_delta(&params.delta).await
        .map_err(|e| ServiceError::new(format!("Failed to submit delta: {}", e)))?;

    // TODO: Create ack signature
    Ok(PushDeltaResult {
        delta: params.delta,
    })
}

/// Get a specific delta
pub async fn get_delta(
    state: &AppState,
    params: GetDeltaParams,
) -> ServiceResult<GetDeltaResult> {
    // Verify account exists
    let metadata = state.metadata.lock().await;
    let account_metadata = metadata.get_account(&params.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {}", e)))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", params.account_id)))?;
    drop(metadata);

    // Verify signature and authorization
    verify_request_auth(
        &account_metadata.auth_type,
        &account_metadata,
        &params.account_id,
        &params.auth.pubkey,
        &params.auth.signature,
    )?;

    // Fetch delta from storage
    let delta = state.storage.pull_delta(&params.account_id, params.nonce).await
        .map_err(|e| ServiceError::new(format!("Failed to fetch delta: {}", e)))?;

    Ok(GetDeltaResult { delta })
}

/// Get the latest delta (head) for an account
pub async fn get_delta_head(
    state: &AppState,
    params: GetDeltaHeadParams,
) -> ServiceResult<GetDeltaHeadResult> {
    // Verify account exists
    let metadata = state.metadata.lock().await;
    let account_metadata = metadata.get_account(&params.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {}", e)))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", params.account_id)))?;
    drop(metadata);

    // Verify signature and authorization
    verify_request_auth(
        &account_metadata.auth_type,
        &account_metadata,
        &params.account_id,
        &params.auth.pubkey,
        &params.auth.signature,
    )?;

    let delta_files = state.storage.list_deltas(&params.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to list deltas: {}", e)))?;

    if delta_files.is_empty() {
        return Err(ServiceError::new(format!("No deltas found for account '{}'", params.account_id)));
    }

    // Parse nonces from filenames and find the maximum
    let mut max_nonce: Option<u64> = None;
    for filename in &delta_files {
        if let Some(nonce_str) = filename.strip_suffix(".json") {
            if let Ok(nonce) = nonce_str.parse::<u64>() {
                max_nonce = Some(max_nonce.map_or(nonce, |current| current.max(nonce)));
            }
        }
    }

    let latest_nonce = max_nonce
        .ok_or_else(|| ServiceError::new("Failed to parse nonces from delta files".to_string()))?;

    // Fetch the latest delta
    let delta = state.storage.pull_delta(&params.account_id, latest_nonce).await
        .map_err(|e| ServiceError::new(format!("Failed to fetch latest delta: {}", e)))?;

    Ok(GetDeltaHeadResult { delta })
}

/// Get the latest nonce for an account (returns None if no deltas exist)
pub async fn get_latest_nonce(
    state: &AppState,
    account_id: &str,
    pubkey: String,
    signature: String,
) -> ServiceResult<Option<u64>> {
    // Verify account exists
    let metadata = state.metadata.lock().await;
    let account_metadata = metadata.get_account(&account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {}", e)))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", account_id)))?;
    drop(metadata);

    // Verify signature and authorization
    verify_request_auth(
        &account_metadata.auth_type,
        &account_metadata,
        account_id,
        &pubkey,
        &signature,
    )?;

    let delta_files = state.storage.list_deltas(account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to list deltas: {}", e)))?;

    if delta_files.is_empty() {
        return Ok(None);
    }

    // Parse nonces from filenames and find the maximum
    let mut max_nonce: Option<u64> = None;
    for filename in &delta_files {
        if let Some(nonce_str) = filename.strip_suffix(".json") {
            if let Ok(nonce) = nonce_str.parse::<u64>() {
                max_nonce = Some(max_nonce.map_or(nonce, |current| current.max(nonce)));
            }
        }
    }

    Ok(max_nonce)
}

/// Get account state
pub async fn get_state(
    state: &AppState,
    params: GetStateParams,
) -> ServiceResult<GetStateResult> {
    // Verify account exists
    let metadata = state.metadata.lock().await;
    let account_metadata = metadata.get_account(&params.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {}", e)))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", &params.account_id)))?;
    drop(metadata);

    // Verify signature and authorization
    verify_request_auth(
        &account_metadata.auth_type,
        &account_metadata,
        &params.account_id,
        &params.auth.pubkey,
        &params.auth.signature,
    )?;

    let account_state = state.storage.pull_state(&params.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to fetch state: {}", e)))?;

    Ok(GetStateResult {
        state: account_state,
    })
}
