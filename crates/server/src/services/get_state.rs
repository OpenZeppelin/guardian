use crate::metadata::auth::Credentials;
use crate::error::{PsmError, Result};
use crate::services::resolve_account;
use crate::state::AppState;
use crate::storage::AccountState;

#[derive(Debug, Clone)]
pub struct GetStateParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetStateResult {
    pub state: AccountState,
}

pub async fn get_state(state: &AppState, params: GetStateParams) -> Result<GetStateResult> {
    let resolved = resolve_account(state, &params.account_id, &params.credentials).await?;

    let account_state = resolved
        .backend
        .pull_state(&params.account_id)
        .await
        .map_err(|_e| PsmError::StateNotFound(params.account_id.clone()))?;

    Ok(GetStateResult {
        state: account_state,
    })
}
