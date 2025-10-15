use crate::auth::Auth;
use crate::state::AppState;
use crate::storage::{AccountMetadata, AccountState, StorageType};

use super::common::{ServiceError, ServiceResult};

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

/// Configure a new account
pub async fn configure_account(
    state: &AppState,
    params: ConfigureAccountParams,
) -> ServiceResult<ConfigureAccountResult> {
    // Validate account ID format based on network type
    state
        .validate_account_id(&params.account_id)
        .map_err(ServiceError::new)?;

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
