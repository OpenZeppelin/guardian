use crate::error::{GuardianError, Result};
use crate::metadata::auth::Credentials;
use crate::modules::disclosure::PolicyEntry;
use crate::services::resolve_account_allowing_session;
use crate::state::AppState;

#[derive(Debug, Clone)]
pub struct GetPoliciesParams {
    pub account_id: String,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct GetPoliciesResult {
    pub account_id: String,
    pub policies: Vec<PolicyEntry>,
}

/// The policies that apply to the account, for its cosigners, filtered by each instance's
/// disclosure (RFC 0002 section 2.7). Empty when no modules are configured.
#[tracing::instrument(level = "info", skip(state, params), fields(account_id = %params.account_id))]
pub async fn get_policies(
    state: &AppState,
    params: GetPoliciesParams,
) -> Result<GetPoliciesResult> {
    let resolved =
        resolve_account_allowing_session(state, &params.account_id, &params.credentials).await?;
    if resolved.metadata.network_config.is_evm() {
        return Err(GuardianError::UnsupportedForNetwork {
            network: "evm".to_string(),
            operation: "get_policies".to_string(),
        });
    }
    let policies = match state.modules.as_deref() {
        Some(modules) => modules.disclosed(&params.account_id).await?,
        None => Vec::new(),
    };
    Ok(GetPoliciesResult {
        account_id: params.account_id.to_ascii_lowercase(),
        policies,
    })
}
