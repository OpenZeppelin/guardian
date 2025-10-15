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
    account_id: String,
    auth_type: AuthType,
    initial_state: serde_json::Value,
    storage_type: String,
    cosigner_pubkeys: Vec<String>,
) -> ServiceResult<()> {
    // Check if account already exists
    let mut metadata = state.metadata.lock().await;
    let existing = metadata.get_account(&account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check existing account: {}", e)))?;

    if existing.is_some() {
        return Err(ServiceError::new(format!("Account '{}' already exists", account_id)));
    }

    // Create initial account state
    let now = chrono::Utc::now().to_rfc3339();
    let account_state = AccountState {
        account_id: account_id.clone(),
        state_json: initial_state,
        commitment: String::new(), // TODO: calculate commitment + validate vs on-chain commitment.
        created_at: now.clone(),
        updated_at: now,
    };

    // Submit initial state to storage
    state.storage.submit_state(&account_state).await
        .map_err(|e| ServiceError::new(format!("Failed to submit initial state: {}", e)))?;

    // Create and store metadata
    let metadata_entry = AccountMetadata {
        account_id: account_id.clone(),
        auth_type,
        storage_type,
        cosigner_pubkeys,
        created_at: account_state.created_at.clone(),
        updated_at: account_state.updated_at.clone(),
    };

    metadata.set_account(metadata_entry).await
        .map_err(|e| ServiceError::new(format!("Failed to store metadata: {}", e)))?;

    Ok(())
}

/// Push a delta
pub async fn push_delta(
    state: &AppState,
    delta: DeltaObject,
    pubkey: String,
    signature: String,
) -> ServiceResult<DeltaObject> {
    // Verify account exists
    let metadata = state.metadata.lock().await;
    let account_metadata = metadata.get_account(&delta.account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {}", e)))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", delta.account_id)))?;
    drop(metadata);

    // Verify signature and authorization
    verify_request_auth(
        &account_metadata.auth_type,
        &account_metadata,
        &delta.account_id,
        &pubkey,
        &signature,
    )?;

    // TODO: Verify prev_commitment matches current state commitment
    // TODO: Verify new commitment vs on-chain commitment in time window.

    // Submit delta to storage
    state.storage.submit_delta(&delta).await
        .map_err(|e| ServiceError::new(format!("Failed to submit delta: {}", e)))?;

    // TODO: Create ack signature
    Ok(delta)
}

/// Get a specific delta
pub async fn get_delta(
    state: &AppState,
    account_id: &str,
    nonce: u64,
    pubkey: String,
    signature: String,
) -> ServiceResult<DeltaObject> {
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

    // Fetch delta from storage
    let delta = state.storage.pull_delta(account_id, nonce).await
        .map_err(|e| ServiceError::new(format!("Failed to fetch delta: {}", e)))?;

    Ok(delta)
}

/// Get the latest delta (head) for an account
pub async fn get_delta_head(
    state: &AppState,
    account_id: &str,
    pubkey: String,
    signature: String,
) -> ServiceResult<DeltaObject> {
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
        return Err(ServiceError::new(format!("No deltas found for account '{}'", account_id)));
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
    let delta = state.storage.pull_delta(account_id, latest_nonce).await
        .map_err(|e| ServiceError::new(format!("Failed to fetch latest delta: {}", e)))?;

    Ok(delta)
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
    account_id: &str,
    pubkey: String,
    signature: String,
) -> ServiceResult<AccountState> {
    // Verify account exists
    let metadata = state.metadata.lock().await;
    let account_metadata = metadata.get_account(&account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {}", e)))?
        .ok_or_else(|| ServiceError::new(format!("Account '{}' not found", &account_id)))?;
    drop(metadata);

    // Verify signature and authorization
    verify_request_auth(
        &account_metadata.auth_type,
        &account_metadata,
        account_id,
        &pubkey,
        &signature,
    )?;

    let account_state = state.storage.pull_state(account_id).await
        .map_err(|e| ServiceError::new(format!("Failed to fetch state: {}", e)))?;

    Ok(account_state)
}
