use crate::auth::Credentials;
use crate::state::AppState;
use crate::storage::DeltaObject;

use super::common::{ServiceError, ServiceResult, verify_request_auth};

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
