use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use private_state_manager_shared::DeltaSignature;
use tracing::info;

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

    // Extract tx_summary and signatures from delta_payload
    let tx_summary = delta_payload
        .get("tx_summary")
        .ok_or_else(|| PsmError::InvalidDelta("Missing 'tx_summary' field".to_string()))?;

    let signatures = delta_payload
        .get("signatures")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    // Validate delta using network client (check validity but don't apply)
    // and compute the delta commitment
    let commitment = {
        let client = state.network_client.lock().await;
        client
            .verify_delta(
                &current_state.commitment,
                &current_state.state_json,
                tx_summary,
            )
            .map_err(PsmError::InvalidDelta)?;

        // Compute the delta proposal ID from the tx_summary
        client
            .delta_proposal_id(&account_id, nonce, tx_summary)
            .map_err(PsmError::InvalidDelta)?
    };

    // Extract proposer ID from credentials
    let proposer_id = match &credentials {
        Credentials::Signature { pubkey, .. } => pubkey.clone(),
    };

    // Parse cosigner signatures from the payload and add timestamp
    let signature_timestamp = state.clock.now_rfc3339();
    let mut cosigner_sigs = Vec::new();
    for sig_value in signatures {
        let parsed: DeltaSignature = serde_json::from_value(sig_value).map_err(|e| {
            PsmError::InvalidDelta(format!("Invalid signature entry in payload: {e}"))
        })?;

        cosigner_sigs.push(CosignerSignature {
            signature: parsed.signature,
            timestamp: signature_timestamp.clone(),
            signer_id: parsed.signer_id,
        });
    }
    let cosigner_ids: Vec<String> = cosigner_sigs
        .iter()
        .map(|sig| sig.signer_id.clone())
        .collect();
    info!(
        account_id = %account_id,
        nonce,
        proposer_id = %proposer_id,
        signer_ids = ?cosigner_ids,
        "push_delta_proposal received"
    );

    // Create delta object with Pending status including any provided signatures
    let timestamp = state.clock.now_rfc3339();
    let delta_proposal = DeltaObject {
        account_id: account_id.clone(),
        nonce,
        prev_commitment: current_state.commitment.clone(),
        new_commitment: None,
        delta_payload,
        ack_sig: None,
        status: DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        },
    };

    // Store the delta proposal in the proposals directory using the commitment as ID
    resolved
        .backend
        .submit_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(PsmError::StorageError)?;
    let stored_signer_count = match &delta_proposal.status {
        DeltaStatus::Pending { cosigner_sigs, .. } => cosigner_sigs.len(),
        _ => 0,
    };
    info!(
        account_id = %account_id,
        nonce,
        commitment = %commitment,
        signer_count = stored_signer_count,
        "push_delta_proposal stored"
    );

    Ok(PushDeltaProposalResult {
        delta: delta_proposal.clone(),
        commitment: commitment.clone(),
    })
}
