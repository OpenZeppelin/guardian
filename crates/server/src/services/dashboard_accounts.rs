use std::cmp::Ordering;

use chrono::DateTime;
use serde::{Deserialize, Serialize};

use crate::error::{GuardianError, Result};
use crate::metadata::{AccountMetadata, auth::Auth};
use crate::state::AppState;
use crate::state_object::StateObject;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardAccountStateStatus {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardAccountSummary {
    pub account_id: String,
    pub auth_scheme: String,
    pub authorized_signer_count: usize,
    pub has_pending_candidate: bool,
    pub current_commitment: Option<String>,
    pub state_status: DashboardAccountStateStatus,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DashboardAccountDetail {
    pub account_id: String,
    pub auth_scheme: String,
    pub authorized_signer_count: usize,
    pub authorized_signer_ids: Vec<String>,
    pub has_pending_candidate: bool,
    pub current_commitment: Option<String>,
    pub state_status: DashboardAccountStateStatus,
    pub created_at: String,
    pub updated_at: String,
    pub state_created_at: Option<String>,
    pub state_updated_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ListDashboardAccountsResult {
    pub accounts: Vec<DashboardAccountSummary>,
    pub total_count: usize,
}

#[derive(Debug, Clone)]
pub struct GetDashboardAccountResult {
    pub account: DashboardAccountDetail,
}

pub async fn list_dashboard_accounts(state: &AppState) -> Result<ListDashboardAccountsResult> {
    let mut accounts = Vec::new();

    for metadata in load_account_metadata(state).await? {
        let (current_commitment, state_status) =
            match state.storage.pull_state(&metadata.account_id).await {
                Ok(account_state) => (
                    Some(account_state.commitment),
                    DashboardAccountStateStatus::Available,
                ),
                Err(error) => {
                    tracing::warn!(
                        account_id = %metadata.account_id,
                        error = %error,
                        "Dashboard account list could not load state"
                    );
                    (None, DashboardAccountStateStatus::Unavailable)
                }
            };

        accounts.push(DashboardAccountSummary::from_parts(
            &metadata,
            current_commitment,
            state_status,
        ));
    }

    accounts.sort_by(compare_summaries);
    let total_count = accounts.len();

    Ok(ListDashboardAccountsResult {
        accounts,
        total_count,
    })
}

pub async fn get_dashboard_account(
    state: &AppState,
    account_id: &str,
) -> Result<GetDashboardAccountResult> {
    let metadata = state
        .metadata
        .get(account_id)
        .await
        .map_err(|error| GuardianError::StorageError(format!("Failed to load metadata: {error}")))?
        .ok_or_else(|| GuardianError::AccountNotFound(account_id.to_string()))?;

    let account_state = state
        .storage
        .pull_state(account_id)
        .await
        .map_err(|error| {
            tracing::warn!(
                account_id = %account_id,
                error = %error,
                "Dashboard account detail could not load state"
            );
            GuardianError::AccountDataUnavailable(account_id.to_string())
        })?;

    Ok(GetDashboardAccountResult {
        account: DashboardAccountDetail::from_parts(&metadata, &account_state),
    })
}

impl DashboardAccountSummary {
    fn from_parts(
        metadata: &AccountMetadata,
        current_commitment: Option<String>,
        state_status: DashboardAccountStateStatus,
    ) -> Self {
        Self {
            account_id: metadata.account_id.clone(),
            auth_scheme: metadata.auth.scheme().to_string(),
            authorized_signer_count: normalized_authorized_signer_ids(&metadata.auth).len(),
            has_pending_candidate: metadata.has_pending_candidate,
            current_commitment,
            state_status,
            created_at: metadata.created_at.clone(),
            updated_at: metadata.updated_at.clone(),
        }
    }
}

impl DashboardAccountDetail {
    fn from_parts(metadata: &AccountMetadata, account_state: &StateObject) -> Self {
        let authorized_signer_ids = normalized_authorized_signer_ids(&metadata.auth);

        Self {
            account_id: metadata.account_id.clone(),
            auth_scheme: metadata.auth.scheme().to_string(),
            authorized_signer_count: authorized_signer_ids.len(),
            authorized_signer_ids,
            has_pending_candidate: metadata.has_pending_candidate,
            current_commitment: Some(account_state.commitment.clone()),
            state_status: DashboardAccountStateStatus::Available,
            created_at: metadata.created_at.clone(),
            updated_at: metadata.updated_at.clone(),
            state_created_at: Some(account_state.created_at.clone()),
            state_updated_at: Some(account_state.updated_at.clone()),
        }
    }
}

async fn load_account_metadata(state: &AppState) -> Result<Vec<AccountMetadata>> {
    let account_ids = state.metadata.list().await.map_err(|error| {
        GuardianError::StorageError(format!("Failed to list metadata: {error}"))
    })?;

    let mut metadata = Vec::with_capacity(account_ids.len());
    for account_id in account_ids {
        let maybe_metadata = state.metadata.get(&account_id).await.map_err(|error| {
            GuardianError::StorageError(format!(
                "Failed to load metadata for account '{}': {}",
                account_id, error
            ))
        })?;
        if let Some(metadata_entry) = maybe_metadata {
            metadata.push(metadata_entry);
        }
    }
    Ok(metadata)
}

fn normalized_authorized_signer_ids(auth: &Auth) -> Vec<String> {
    let mut signer_ids = match auth {
        Auth::MidenFalconRpo {
            cosigner_commitments,
        }
        | Auth::MidenEcdsa {
            cosigner_commitments,
        } => cosigner_commitments.clone(),
        Auth::EvmEcdsa { signers } => signers.clone(),
    };
    signer_ids.sort();
    signer_ids.dedup();
    signer_ids
}

fn compare_summaries(left: &DashboardAccountSummary, right: &DashboardAccountSummary) -> Ordering {
    compare_timestamps_desc(&left.updated_at, &right.updated_at)
        .then_with(|| left.account_id.cmp(&right.account_id))
}

fn compare_timestamps_desc(left: &str, right: &str) -> Ordering {
    match (parse_timestamp(left), parse_timestamp(right)) {
        (Some(left_ts), Some(right_ts)) => right_ts.cmp(&left_ts),
        _ => right.cmp(left),
    }
}

fn parse_timestamp(value: &str) -> Option<DateTime<chrono::FixedOffset>> {
    DateTime::parse_from_rfc3339(value).ok()
}
