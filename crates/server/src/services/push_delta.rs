use crate::auth::Credentials;
use crate::canonicalization::CanonicalizationMode;
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject};

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

    // Check if there are any pending candidate deltas (non-canonical)
    let all_deltas = storage_backend
        .pull_deltas_after(&params.delta.account_id, 0)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to check deltas: {e}")))?;

    eprintln!("DEBUG: Checking {} existing deltas", all_deltas.len());
    for delta in &all_deltas {
        eprintln!(
            "  Delta {}: candidate_at={:?}, canonical_at={:?}, discarded_at={:?}",
            delta.nonce, delta.candidate_at, delta.canonical_at, delta.discarded_at
        );
    }

    let has_pending_candidate = all_deltas
        .iter()
        .any(|d| d.candidate_at.is_some() && d.canonical_at.is_none() && d.discarded_at.is_none());

    eprintln!("DEBUG: has_pending_candidate = {has_pending_candidate}");

    if has_pending_candidate {
        return Err(ServiceError::new(
            "Cannot push new delta: there is already a non-canonical delta pending. Wait for canonicalization or discard the pending delta.".to_string()
        ));
    }

    // Fetch current account state
    let current_state = storage_backend
        .pull_state(&params.delta.account_id)
        .await
        .map_err(|e| ServiceError::new(format!("Failed to fetch account state: {e}")))?;

    // Verify prev_commitment matches current state
    if params.delta.prev_commitment != current_state.commitment {
        return Err(ServiceError::new(format!(
            "Delta prev_commitment mismatch: expected {}, got {}",
            current_state.commitment, params.delta.prev_commitment
        )));
    }

    {
        let client = state.network_client.lock().await;
        client
            .verify_delta(
                &params.delta.prev_commitment,
                &params.delta.new_commitment,
                &current_state.state_json,
                &params.delta.delta_payload,
            )
            .map_err(|e| ServiceError::new(format!("Delta verification failed: {e}")))?;
    }

    let now = chrono::Utc::now().to_rfc3339();

    // Handle based on canonicalization mode
    match &state.canonicalization_mode {
        CanonicalizationMode::Enabled(_config) => {
            // Submit delta as candidate
            // The canonicalization worker will verify on-chain and apply state updates later
            let mut candidate_delta = params.delta.clone();
            candidate_delta.candidate_at = Some(now);

            storage_backend
                .submit_delta(&candidate_delta)
                .await
                .map_err(|e| ServiceError::new(format!("Failed to submit delta: {e}")))?;
        }
        CanonicalizationMode::Optimistic => {
            // Optimistically accept delta as canonical immediately
            // Apply state update without on-chain verification
            let mut canonical_delta = params.delta.clone();
            canonical_delta.candidate_at = Some(now.clone());
            canonical_delta.canonical_at = Some(now.clone());

            let (new_state_json, new_commitment) = {
                let client = state.network_client.lock().await;
                client
                    .apply_delta(&current_state.state_json, &canonical_delta.delta_payload)
                    .map_err(|e| ServiceError::new(format!("Failed to apply delta: {e}")))?
            };

            // Update account state
            let new_state = AccountState {
                account_id: canonical_delta.account_id.clone(),
                commitment: new_commitment,
                state_json: new_state_json,
                created_at: current_state.created_at,
                updated_at: now,
            };

            storage_backend
                .submit_state(&new_state)
                .await
                .map_err(|e| ServiceError::new(format!("Failed to update state: {e}")))?;

            // Store delta as canonical
            storage_backend
                .submit_delta(&canonical_delta)
                .await
                .map_err(|e| ServiceError::new(format!("Failed to submit delta: {e}")))?;
        }
    }

    // TODO: Create ack signature
    Ok(PushDeltaResult {
        delta: params.delta,
    })
}
