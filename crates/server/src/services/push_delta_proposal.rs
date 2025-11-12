use crate::builder::state::AppState;
use crate::delta_object::DeltaObject;
use crate::delta_object::DeltaStatus;
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;

#[derive(Debug, Clone)]
pub struct PushDeltaProposalParams {
    pub account_id: String,
    pub nonce: u64,
    pub delta_payload: serde_json::Value,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct PushDeltaProposalResult {
    pub delta: DeltaObject,
    pub commitment: String,
}

pub async fn push_delta_proposal(
    state: &AppState,
    params: PushDeltaProposalParams,
) -> Result<PushDeltaProposalResult> {
    let PushDeltaProposalParams {
        account_id,
        nonce,
        delta_payload,
        credentials,
    } = params;

    // Resolve account and verify authentication
    let resolved = resolve_account(state, &account_id, &credentials).await?;

    // Fetch current state to validate delta
    let current_state = resolved
        .backend
        .pull_state(&account_id)
        .await
        .map_err(|_| PsmError::StateNotFound(account_id.clone()))?;

    // Validate delta using network client (check validity but don't apply)
    // and compute the delta commitment
    let commitment = {
        let client = state.network_client.lock().await;
        client
            .verify_delta(
                &current_state.commitment,
                &current_state.state_json,
                &delta_payload,
            )
            .map_err(PsmError::InvalidDelta)?;

        // Compute the delta proposal ID
        client
            .delta_proposal_id(&account_id, nonce, &delta_payload)
            .map_err(PsmError::InvalidDelta)?
    };

    // Extract proposer ID from credentials
    let proposer_id = match &credentials {
        Credentials::Signature { pubkey, .. } => pubkey.clone(),
    };

    // Create delta object with Pending status
    let timestamp = state.clock.now_rfc3339();
    let delta_proposal = DeltaObject {
        account_id: account_id.clone(),
        nonce,
        prev_commitment: current_state.commitment.clone(), // Use actual state commitment for validation
        new_commitment: None,
        delta_payload,
        ack_sig: None,
        status: DeltaStatus::pending(timestamp, proposer_id),
    };

    // Store the delta proposal in the proposals directory using the commitment as ID
    resolved
        .backend
        .submit_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(PsmError::StorageError)?;

    Ok(PushDeltaProposalResult {
        delta: delta_proposal.clone(),
        commitment: commitment.clone(),
    })
}
