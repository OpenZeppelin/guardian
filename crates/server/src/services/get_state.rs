use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use crate::state::AppState;
use crate::state_object::StateObject;

#[derive(Debug, Clone)]
pub struct GetStateParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetStateResult {
    pub state: StateObject,
}

#[tracing::instrument(
    skip(state, params),
    fields(account_id = %params.account_id)
)]
pub async fn get_state(state: &AppState, params: GetStateParams) -> Result<GetStateResult> {
    tracing::info!(account_id = %params.account_id, "Getting state");

    let resolved = resolve_account(state, &params.account_id, &params.credentials).await?;

    let account_state = resolved
        .storage
        .pull_state(&params.account_id)
        .await
        .map_err(|_e| PsmError::StateNotFound(params.account_id.clone()))?;

    Ok(GetStateResult {
        state: account_state,
    })
}
