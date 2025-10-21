use crate::auth::Credentials;
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::storage::DeltaObject;

#[derive(Debug, Clone)]
pub struct GetDeltaHeadParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaHeadResult {
    pub delta: DeltaObject,
}

pub async fn get_delta_head(
    state: &AppState,
    params: GetDeltaHeadParams,
) -> Result<GetDeltaHeadResult> {
    let account_metadata = state
        .metadata
        .get(&params.account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to check account: {e}")))?
        .ok_or_else(|| PsmError::AccountNotFound(params.account_id.clone()))?;

    account_metadata
        .auth
        .verify(&params.account_id, &params.credentials)
        .map_err(PsmError::AuthenticationFailed)?;

    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(PsmError::ConfigurationError)?;

    let all_deltas = storage_backend
        .pull_deltas_after(&params.account_id, 0)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to fetch deltas: {e}")))?;

    let delta = all_deltas
        .into_iter()
        .filter(|d| !d.status.is_discarded())
        .max_by_key(|d| d.nonce)
        .ok_or_else(|| PsmError::DeltaNotFound {
            account_id: params.account_id.clone(),
            nonce: 0,
        })?;

    Ok(GetDeltaHeadResult { delta })
}
