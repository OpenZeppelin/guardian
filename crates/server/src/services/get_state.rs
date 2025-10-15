use crate::auth::Credentials;
use crate::state::AppState;
use crate::storage::AccountState;

use super::common::{ServiceError, ServiceResult, verify_request_auth};

#[derive(Debug, Clone)]
pub struct GetStateParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetStateResult {
    pub state: AccountState,
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
        &params.account_id,
        &params.credentials,
    )?;

    // Get the storage backend for this account
    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(ServiceError::new)?;

    let account_state = storage_backend
        .pull_state(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to fetch state: {e}")))?;

    Ok(GetStateResult {
        state: account_state,
    })
}
