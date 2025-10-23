use crate::error::{PsmError, Result};
use crate::metadata::AccountMetadata;
use crate::metadata::auth::Credentials;
use crate::state::AppState;
use crate::storage::StorageBackend;
use std::sync::Arc;

mod configure_account;
mod get_delta;
mod get_delta_since;
mod get_state;
mod push_delta;

pub use crate::jobs::canonicalization::{
    process_canonicalizations_now, start_canonicalization_worker,
};
pub use configure_account::{ConfigureAccountParams, ConfigureAccountResult, configure_account};
pub use get_delta::{GetDeltaParams, GetDeltaResult, get_delta};
pub use get_delta_since::{GetDeltaSinceParams, GetDeltaSinceResult, get_delta_since};
pub use get_state::{GetStateParams, GetStateResult, get_state};
pub use push_delta::{PushDeltaParams, PushDeltaResult, push_delta};

#[derive(Clone)]
pub struct ResolvedAccount {
    pub metadata: AccountMetadata,
    pub backend: Arc<dyn StorageBackend>,
}

pub async fn resolve_account(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
) -> Result<ResolvedAccount> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to check account: {e}")))?
        .ok_or_else(|| PsmError::AccountNotFound(account_id.to_string()))?;

    metadata
        .auth
        .verify(account_id, creds)
        .map_err(PsmError::AuthenticationFailed)?;

    let backend = state
        .storage
        .get(&metadata.storage_type)
        .map_err(PsmError::ConfigurationError)?;

    Ok(ResolvedAccount { metadata, backend })
}
