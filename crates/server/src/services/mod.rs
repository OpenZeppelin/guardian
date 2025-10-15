use crate::auth::{Auth, Credentials};
use crate::state::AppState;
use crate::storage::{AccountMetadata, AccountState, DeltaObject, StorageType};

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

#[derive(Debug, Clone)]
pub struct ConfigureAccountParams {
    pub account_id: String,
    pub auth: Auth,
    pub initial_state: serde_json::Value,
    pub storage_type: StorageType,
    pub cosigner_pubkeys: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ConfigureAccountResult {
    pub account_id: String,
}

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

#[derive(Debug, Clone)]
pub struct GetDeltaParams {
    pub account_id: String,
    pub nonce: u64,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaResult {
    pub delta: DeltaObject,
}

#[derive(Debug, Clone)]
pub struct GetDeltaHeadParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaHeadResult {
    pub delta: DeltaObject,
}

#[derive(Debug, Clone)]
pub struct GetStateParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetStateResult {
    pub state: AccountState,
}

/// Verify credentials and authorization for a request
fn verify_request_auth(
    auth: &Auth,
    account_metadata: &AccountMetadata,
    account_id: &str,
    credentials: &Credentials,
) -> ServiceResult<()> {
    auth.verify(account_id, credentials, account_metadata)
        .map_err(|e| ServiceError::new(format!("Authentication failed: {e}")))
}

/// Configure a new account
pub async fn configure_account(
    state: &AppState,
    params: ConfigureAccountParams,
) -> ServiceResult<ConfigureAccountResult> {
    // Check if account already exists
    let existing = state
        .metadata
        .get(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check existing account: {e}")))?;

    if existing.is_some() {
        return Err(ServiceError::new(format!(
            "Account '{}' already exists",
            params.account_id
        )));
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
    state
        .storage
        .submit_state(&account_state)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to submit initial state: {e}")))?;

    // Create and store metadata
    let metadata_entry = AccountMetadata {
        account_id: params.account_id.clone(),
        auth: params.auth,
        storage_type: params.storage_type,
        cosigner_pubkeys: params.cosigner_pubkeys,
        created_at: account_state.created_at.clone(),
        updated_at: account_state.updated_at.clone(),
    };

    state
        .metadata
        .set(metadata_entry)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to store metadata: {e}")))?;

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
    let account_metadata = state
        .metadata
        .get(&params.delta.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {e}")))?
        .ok_or_else(|| {
            ServiceError::new(format!("Account '{}' not found", params.delta.account_id))
        })?;

    // Verify authentication and authorization
    verify_request_auth(
        &account_metadata.auth,
        &account_metadata,
        &params.delta.account_id,
        &params.credentials,
    )?;

    // TODO: Verify prev_commitment matches current state commitment
    // TODO: Verify new commitment vs on-chain commitment in time window.

    // Submit delta to storage
    state
        .storage
        .submit_delta(&params.delta)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to submit delta: {e}")))?;

    // TODO: Create ack signature
    Ok(PushDeltaResult {
        delta: params.delta,
    })
}

/// Get a specific delta
pub async fn get_delta(state: &AppState, params: GetDeltaParams) -> ServiceResult<GetDeltaResult> {
    // Verify account exists
    let account_metadata = state
        .metadata
        .get(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {e}")))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", params.account_id)))?;

    // Verify authentication and authorization
    verify_request_auth(
        &account_metadata.auth,
        &account_metadata,
        &params.account_id,
        &params.credentials,
    )?;

    // Fetch delta from storage
    let delta = state
        .storage
        .pull_delta(&params.account_id, params.nonce)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to fetch delta: {e}")))?;

    Ok(GetDeltaResult { delta })
}

/// Get the latest delta (head) for an account
pub async fn get_delta_head(
    state: &AppState,
    params: GetDeltaHeadParams,
) -> ServiceResult<GetDeltaHeadResult> {
    // Verify account exists
    let account_metadata = state
        .metadata
        .get(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {e}")))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", params.account_id)))?;

    // Verify authentication and authorization
    verify_request_auth(
        &account_metadata.auth,
        &account_metadata,
        &params.account_id,
        &params.credentials,
    )?;

    let delta_files = state
        .storage
        .list_deltas(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to list deltas: {e}")))?;

    if delta_files.is_empty() {
        return Err(ServiceError::new(format!(
            "No deltas found for account '{}'",
            params.account_id
        )));
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
    let delta = state
        .storage
        .pull_delta(&params.account_id, latest_nonce)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to fetch latest delta: {e}")))?;

    Ok(GetDeltaHeadResult { delta })
}

/// Get the latest nonce for an account (returns None if no deltas exist)
pub async fn get_latest_nonce(
    state: &AppState,
    account_id: &str,
    credentials: Credentials,
) -> ServiceResult<Option<u64>> {
    // Verify account exists
    let account_metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {e}")))?
        .ok_or_else(|| ServiceError::new(format!("Account '{account_id}' not found")))?;

    // Verify authentication and authorization
    verify_request_auth(
        &account_metadata.auth,
        &account_metadata,
        account_id,
        &credentials,
    )?;

    let delta_files = state
        .storage
        .list_deltas(account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to list deltas: {e}")))?;

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
pub async fn get_state(state: &AppState, params: GetStateParams) -> ServiceResult<GetStateResult> {
    // Verify account exists
    let account_metadata = state
        .metadata
        .get(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {e}")))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", &params.account_id)))?;

    // Verify authentication and authorization
    verify_request_auth(
        &account_metadata.auth,
        &account_metadata,
        &params.account_id,
        &params.credentials,
    )?;

    let account_state = state
        .storage
        .pull_state(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to fetch state: {e}")))?;

    Ok(GetStateResult {
        state: account_state,
    })
}
