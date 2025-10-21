use crate::auth::Credentials;
use crate::canonicalization::CanonicalizationMode;
use crate::error::{PsmError, Result};
use crate::state::AppState;
use crate::storage::{AccountState, DeltaObject, DeltaStatus, StorageBackend};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

pub async fn push_delta(
    state: &AppState,
    params: PushDeltaParams,
) -> Result<PushDeltaResult> {
    let account_metadata = state
        .metadata
        .get(&params.delta.account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to check account: {e}")))?
        .ok_or_else(|| PsmError::AccountNotFound(params.delta.account_id.clone()))?;

    account_metadata
        .auth
        .verify(&params.delta.account_id, &params.credentials)
        .map_err(PsmError::AuthenticationFailed)?;

    let storage_backend = state
        .storage
        .get(&account_metadata.storage_type)
        .map_err(PsmError::ConfigurationError)?;

    check_no_pending_candidates(&storage_backend, &params.delta.account_id).await?;

    let current_state = storage_backend
        .pull_state(&params.delta.account_id)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to fetch account state: {e}")))?;

    verify_prev_commitment(&params.delta, &current_state)?;

    let new_commitment =
        calculate_new_commitment(state, &current_state, &params.delta.delta_payload).await?;

    let now = chrono::Utc::now().to_rfc3339();

    match &state.canonicalization_mode {
        CanonicalizationMode::Enabled(_) => {
            save_as_candidate(&storage_backend, &params.delta, &new_commitment, &now).await?;
        }
        CanonicalizationMode::Optimistic => {
            save_as_canonical(
                state,
                &storage_backend,
                &params.delta,
                &current_state,
                &new_commitment,
                &now,
            )
            .await?;
        }
    }

    Ok(PushDeltaResult {
        delta: params.delta,
    })
}

async fn check_no_pending_candidates(
    storage_backend: &Arc<dyn StorageBackend>,
    account_id: &str,
) -> Result<()> {
    let all_deltas = storage_backend
        .pull_deltas_after(account_id, 0)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to check deltas: {e}")))?;

    eprintln!("DEBUG: Checking {} existing deltas", all_deltas.len());
    for delta in &all_deltas {
        eprintln!("  Delta {}: status={:?}", delta.nonce, delta.status);
    }

    let has_pending_candidate = all_deltas.iter().any(|d| d.status.is_candidate());

    eprintln!("DEBUG: has_pending_candidate = {has_pending_candidate}");

    if has_pending_candidate {
        return Err(PsmError::ConflictPendingDelta);
    }

    Ok(())
}

fn verify_prev_commitment(delta: &DeltaObject, current_state: &AccountState) -> Result<()> {
    if delta.prev_commitment != current_state.commitment {
        return Err(PsmError::CommitmentMismatch {
            expected: current_state.commitment.clone(),
            actual: delta.prev_commitment.clone(),
        });
    }
    Ok(())
}

async fn calculate_new_commitment(
    state: &AppState,
    current_state: &AccountState,
    delta_payload: &serde_json::Value,
) -> Result<String> {
    let client = state.network_client.lock().await;

    client
        .verify_delta(
            &current_state.commitment,
            &current_state.state_json,
            delta_payload,
        )
        .map_err(PsmError::InvalidDelta)?;

    let (_, commitment) = client
        .apply_delta(&current_state.state_json, delta_payload)
        .map_err(PsmError::InvalidDelta)?;

    Ok(commitment)
}

async fn save_as_candidate(
    storage_backend: &Arc<dyn StorageBackend>,
    delta: &DeltaObject,
    new_commitment: &str,
    timestamp: &str,
) -> Result<()> {
    let mut candidate_delta = delta.clone();
    candidate_delta.new_commitment = new_commitment.to_string();
    candidate_delta.status = DeltaStatus::candidate(timestamp.to_string());

    storage_backend
        .submit_delta(&candidate_delta)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to submit delta: {e}")))
}

async fn save_as_canonical(
    state: &AppState,
    storage_backend: &Arc<dyn StorageBackend>,
    delta: &DeltaObject,
    current_state: &AccountState,
    new_commitment: &str,
    timestamp: &str,
) -> Result<()> {
    let mut canonical_delta = delta.clone();
    canonical_delta.new_commitment = new_commitment.to_string();
    canonical_delta.status = DeltaStatus::canonical(timestamp.to_string());

    let (new_state_json, _) = {
        let client = state.network_client.lock().await;
        client
            .apply_delta(&current_state.state_json, &canonical_delta.delta_payload)
            .map_err(PsmError::InvalidDelta)?
    };

    let new_state = AccountState {
        account_id: canonical_delta.account_id.clone(),
        commitment: new_commitment.to_string(),
        state_json: new_state_json,
        created_at: current_state.created_at.clone(),
        updated_at: timestamp.to_string(),
    };

    storage_backend
        .submit_state(&new_state)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to update state: {e}")))?;

    storage_backend
        .submit_delta(&canonical_delta)
        .await
        .map_err(|e| PsmError::StorageError(format!("Failed to submit delta: {e}")))?;

    Ok(())
}
