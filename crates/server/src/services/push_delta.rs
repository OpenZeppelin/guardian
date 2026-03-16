use private_state_manager_shared::SignatureScheme;

use crate::delta_object::DeltaObject;
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::delta_commit::{CommitContext, DeltaCommitStrategy};
use crate::services::resolve_account;
use crate::state::AppState;

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
        .storage
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
    let has_pending = resolved
        .storage
        .has_pending_candidate(&params.delta.account_id)
        .await
        .map_err(|e| {
            tracing::error!(
                account_id = %params.delta.account_id,
                error = %e,
                "Failed to check deltas in push_delta"
            );
            PsmError::StorageError(format!("Failed to check deltas: {e}"))
        })?;

    if has_pending {
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
    result_delta.new_commitment = Some(new_commitment.clone());
    let scheme = resolved.metadata.auth.scheme();
    result_delta = state.ack.ack_delta(result_delta, &scheme)?;
    result_delta.ack_pubkey = state.ack.pubkey(&scheme);
    result_delta.ack_scheme = match scheme {
        SignatureScheme::Falcon => "falcon",
        SignatureScheme::Ecdsa => "ecdsa",
    }
    .to_string();

    let now = state.clock.now_rfc3339();
    let commit_strategy = DeltaCommitStrategy::from_app_state(state);
    commit_strategy
        .commit(
            CommitContext {
                state,
                resolved: &resolved,
                current_state: &current_state,
                now,
            },
            &mut result_delta,
            new_state_json,
            &new_commitment,
        )
        .await?;

    Ok(PushDeltaResult {
        delta: result_delta,
    })
}
