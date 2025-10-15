use crate::auth::Credentials;
use crate::state::AppState;
use crate::storage::DeltaObject;

use super::common::{ServiceError, ServiceResult, verify_request_auth};

#[derive(Debug, Clone)]
pub struct GetDeltaHeadParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaHeadResult {
    pub delta: DeltaObject,
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
        &params.account_id,
        &params.credentials,
    )?;

    // Get the storage backend for this account
    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(ServiceError::new)?;

    // Get the latest nonce from storage
    let latest_nonce = storage_backend
        .get_delta_head(&params.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to get latest nonce: {e}")))?
        .ok_or_else(|| {
            ServiceError::new(format!(
                "No deltas found for account '{}'",
                params.account_id
            ))
        })?;

    // Fetch the latest delta
    let delta = storage_backend
        .pull_delta(&params.account_id, latest_nonce)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to fetch latest delta: {e}")))?;

    Ok(GetDeltaHeadResult { delta })
}
