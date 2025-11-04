use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use crate::state::AppState;
use crate::state_object::StateObject;

#[derive(Debug, Clone)]
pub struct PushDeltaParams {
    pub delta: DeltaObject,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaResult {
    pub delta: DeltaObject,
}

#[tracing::instrument(
    skip(state, params),
    fields(account_id = %params.delta.account_id)
)]
pub async fn push_delta(state: &AppState, params: PushDeltaParams) -> Result<PushDeltaResult> {
    tracing::info!(account_id = %params.delta.account_id, "Pushing delta");

    let resolved = resolve_account(state, &params.delta.account_id, &params.credentials).await?;

    let current_state = resolved
        .backend
        .pull_state(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to fetch account state in push_delta"
            );
            PsmError::StorageError(format!("Failed to fetch account state: {e}"))
        })?;

    // Check for pending candidates before accepting new delta
    let all_deltas = resolved
        .backend
        .pull_deltas_after(&params.delta.account_id, 0)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to check deltas in push_delta"
            );
            PsmError::StorageError(format!("Failed to check deltas: {e}"))
        })?;

    if all_deltas.iter().any(|d| d.status.is_candidate()) {
        return Err(PsmError::ConflictPendingDelta);
    }

    if params.delta.prev_commitment != current_state.commitment {
        return Err(PsmError::CommitmentMismatch {
            expected: current_state.commitment.clone(),
            actual: params.delta.prev_commitment.clone(),
        });
    }

    let (new_state_json, new_commitment) = {
        let client = state.network_client.lock().await;
        client
            .verify_delta(
                &current_state.commitment,
                &current_state.state_json,
                &params.delta.delta_payload,
            )
            .map_err(PsmError::InvalidDelta)?;
        client
            .apply_delta(&current_state.state_json, &params.delta.delta_payload)
            .map_err(PsmError::InvalidDelta)?
    };

    let mut result_delta = params.delta.clone();
    result_delta.new_commitment = new_commitment;
    result_delta = state.ack.ack_delta(result_delta)?;

    let now = state.clock.now_rfc3339();

    if state.canonicalization.is_some() {
        result_delta.status = DeltaStatus::candidate(now);
        resolved
            .backend
            .submit_delta(&result_delta)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.delta.account_id,
                    nonce = result_delta.nonce,
                    error = %e,
                    "Failed to submit candidate delta"
                );
                PsmError::StorageError(format!("Failed to submit delta: {e}"))
            })?;
    } else {
        result_delta.status = DeltaStatus::canonical(now.clone());

        let new_state = StateObject {
            account_id: result_delta.account_id.clone(),
            commitment: result_delta.new_commitment.clone(),
            state_json: new_state_json,
            created_at: current_state.created_at.clone(),
            updated_at: now,
        };

        resolved
            .backend
            .submit_state(&new_state)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.delta.account_id,
                    error = %e,
                    "Failed to update state in optimistic mode"
                );
                PsmError::StorageError(format!("Failed to update state: {e}"))
            })?;
        resolved
            .backend
            .submit_delta(&result_delta)
            .await
            .map_err(|e| {
                tracing::error!(
                    account_id = %params.delta.account_id,
                    nonce = result_delta.nonce,
                    error = %e,
                    "Failed to submit canonical delta in optimistic mode"
                );
                PsmError::StorageError(format!("Failed to submit delta: {e}"))
            })?;
    }

    Ok(PushDeltaResult {
        delta: result_delta,
    })
}
