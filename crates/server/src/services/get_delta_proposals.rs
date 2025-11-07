use crate::delta_object::DeltaObject;
use crate::error::Result;
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use crate::builder::state::AppState;

#[derive(Debug, Clone)]
pub struct GetDeltaProposalsParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetDeltaProposalsResult {
    pub proposals: Vec<DeltaObject>,
}

pub async fn get_delta_proposals(
    state: &AppState,
    params: GetDeltaProposalsParams,
) -> Result<GetDeltaProposalsResult> {
    let GetDeltaProposalsParams {
        account_id,
        credentials,
    } = params;

    // Resolve account and verify authentication
    let resolved = resolve_account(state, &account_id, &credentials).await?;

    // Get all proposals from the proposals directory
    let mut proposals = resolved
        .backend
        .pull_all_delta_proposals(&account_id)
        .await
        .unwrap_or_default();

    // Filter by status::Pending and sort by nonce
    proposals.retain(|p| p.status.is_pending());
    proposals.sort_by_key(|p| p.nonce);

    Ok(GetDeltaProposalsResult { proposals })
}