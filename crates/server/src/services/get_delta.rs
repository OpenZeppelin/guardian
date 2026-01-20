use crate::delta_object::DeltaObject;
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use crate::state::AppState;

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
#[tracing::instrument(
    skip(state, params),
    fields(account_id = %params.account_id, nonce = params.nonce)
)]
pub async fn get_delta(state: &AppState, params: GetDeltaParams) -> Result<GetDeltaResult> {
    tracing::info!(account_id = %params.account_id, nonce = params.nonce, "Getting delta");

    let resolved = resolve_account(state, &params.account_id, &params.credentials).await?;

    let delta = resolved
        .storage
        .pull_delta(&params.account_id, params.nonce)
        .await
        .map_err(|_e| PsmError::DeltaNotFound {
            account_id: params.account_id.clone(),
            nonce: params.nonce,
        })?;

    Ok(GetDeltaResult { delta })
}
