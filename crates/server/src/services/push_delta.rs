use crate::auth::Credentials;
use crate::state::AppState;
use crate::storage::DeltaObject;

use super::common::{ServiceError, ServiceResult, verify_request_auth};

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

/// Push a delta
pub async fn push_delta(
    state: &AppState,
    params: PushDeltaParams,
) -> ServiceResult<PushDeltaResult> {
    // Verify account exists
    let account_metadata = state
        .metadata
        .get(&params.delta.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check account: {e}")))?
        .ok_or_else(|| {
            ServiceError::new(format!("Account '{}' not found", params.delta.account_id))
        })?;

    // Verify authentication and authorization
    verify_request_auth(
        &account_metadata.auth,
        &params.delta.account_id,
        &params.credentials,
    )?;

    // Get the storage backend for this account
    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(ServiceError::new)?;

    // Fetch current account state
    let current_state = storage_backend
        .pull_state(&params.delta.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to fetch account state: {e}")))?;

    // Verify commitments and apply delta
    let (new_state_json, new_commitment) = {
        let client = state.network_client.lock().await;
        client
            .verify_and_apply_delta(
                &params.delta.prev_commitment,
                &params.delta.new_commitment,
                &current_state.state_json,
                &params.delta.delta_payload,
            )
            .map_err(|e| ServiceError::new(format!("Delta verification failed: {e}")))?
    };

    // Submit delta to storage
    storage_backend
        .submit_delta(&params.delta)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to submit delta: {e}")))?;

    // TODO: after canonicalization.
    // Update account state with new commitment
    let now = chrono::Utc::now().to_rfc3339();
    let updated_state = crate::storage::AccountState {
        account_id: params.delta.account_id.clone(),
        state_json: new_state_json,
        commitment: new_commitment,
        created_at: current_state.created_at.clone(),
        updated_at: now,
    };

    storage_backend
        .submit_state(&updated_state)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to update account state: {e}")))?;

    // TODO: Verify new commitment vs on-chain commitment in time window.

    // TODO: Create ack signature
    Ok(PushDeltaResult {
        delta: params.delta,
    })
}
