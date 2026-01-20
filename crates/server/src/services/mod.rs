use crate::error::{PsmError, Result};
use crate::metadata::AccountMetadata;
use crate::metadata::auth::Credentials;
use crate::state::AppState;
use crate::storage::StorageBackend;
use std::sync::Arc;

mod configure_account;
mod delta_commit;
mod get_delta;
mod get_delta_proposals;
mod get_delta_since;
mod get_state;
mod payload_normalize;
mod push_delta;
mod push_delta_proposal;
mod sign_delta_proposal;

pub use crate::jobs::canonicalization::{
    process_canonicalizations_now, start_canonicalization_worker,
};
pub use configure_account::{ConfigureAccountParams, ConfigureAccountResult, configure_account};
pub use get_delta::{GetDeltaParams, GetDeltaResult, get_delta};
pub use get_delta_proposals::{
    GetDeltaProposalsParams, GetDeltaProposalsResult, get_delta_proposals,
};
pub use get_delta_since::{GetDeltaSinceParams, GetDeltaSinceResult, get_delta_since};
pub use get_state::{GetStateParams, GetStateResult, get_state};
pub use payload_normalize::normalize_payload;
pub use push_delta::{PushDeltaParams, PushDeltaResult, push_delta};
pub use push_delta_proposal::{
    PushDeltaProposalParams, PushDeltaProposalResult, push_delta_proposal,
};
pub use sign_delta_proposal::{
    SignDeltaProposalParams, SignDeltaProposalResult, sign_delta_proposal,
};

#[derive(Clone)]
pub struct ResolvedAccount {
    pub metadata: AccountMetadata,
    pub storage: Arc<dyn StorageBackend>,
}

#[tracing::instrument(skip(state, creds), fields(account_id = %account_id))]
pub async fn resolve_account(
    state: &AppState,
    account_id: &str,
    creds: &Credentials,
) -> Result<ResolvedAccount> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %account_id,
                error = %e,
                "Failed to check account in resolve_account"
            );
            PsmError::StorageError(format!("Failed to check account: {e}"))
        })?
        .ok_or_else(|| PsmError::AccountNotFound(account_id.to_string()))?;

    metadata.auth.verify(account_id, creds).map_err(|e| {
        tracing::warn!(
            account_id = %account_id,
            error = %e,
            "Authentication failed in resolve_account"
        );
        PsmError::AuthenticationFailed(e)
    })?;

    let storage = state.storage.clone();

    Ok(ResolvedAccount { metadata, storage })
}
