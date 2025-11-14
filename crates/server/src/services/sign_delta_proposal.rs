use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus, ProposalSignature};
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;
use miden_objects::crypto::dsa::rpo_falcon512::PublicKey;
use miden_objects::utils::Serializable;
use private_state_manager_shared::hex::FromHex;
use tracing::info;

#[derive(Debug, Clone)]
pub struct SignDeltaProposalParams {
    pub account_id: String,
    pub commitment: String,
    pub signature: ProposalSignature,
    pub credentials: Credentials,
}

#[derive(Debug, Clone)]
pub struct SignDeltaProposalResult {
    pub delta: DeltaObject,
}

pub async fn sign_delta_proposal(
    state: &AppState,
    params: SignDeltaProposalParams,
) -> Result<SignDeltaProposalResult> {
    let SignDeltaProposalParams {
        account_id,
        commitment,
        signature,
        credentials,
    } = params;

    // Resolve account and verify authentication
    let resolved = resolve_account(state, &account_id, &credentials).await?;

    // Fetch the proposal by commitment
    let mut delta_proposal = resolved
        .backend
        .pull_delta_proposal(&account_id, &commitment)
        .await
        .map_err(|_| PsmError::ProposalNotFound {
            account_id: account_id.clone(),
            commitment: commitment.clone(),
        })?;

    // Verify is a pending proposal
    let (timestamp, proposer_id, mut cosigner_sigs) = match &delta_proposal.status {
        DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        } => (
            timestamp.clone(),
            proposer_id.clone(),
            cosigner_sigs.clone(),
        ),
        _ => {
            return Err(PsmError::ProposalNotFound {
                account_id: account_id.clone(),
                commitment: commitment.clone(),
            });
        }
    };

    // Extract signer ID from credentials
    let signer_commitment_hex = match &credentials {
        Credentials::Signature { pubkey, .. } => {
            let public_key = PublicKey::from_hex(pubkey).map_err(|e| {
                PsmError::AuthenticationFailed(format!(
                    "invalid signer public key for {}: {}",
                    account_id, e
                ))
            })?;
            let commitment = public_key.to_commitment();
            format!("0x{}", hex::encode(commitment.to_bytes()))
        }
    };

    // Check if already signed by this signer
    if cosigner_sigs
        .iter()
        .any(|sig| sig.signer_id.eq_ignore_ascii_case(&signer_commitment_hex))
    {
        return Err(PsmError::ProposalAlreadySigned {
            signer_id: signer_commitment_hex.clone(),
        });
    }

    // Create the proposal signature based on scheme
    // Add the new signature
    let new_signature = CosignerSignature {
        signature,
        timestamp: state.clock.now_rfc3339(),
        signer_id: signer_commitment_hex.clone(),
    };
    cosigner_sigs.push(new_signature);

    info!(
        account_id = %account_id,
        signer_commitment = %signer_commitment_hex,
        total_signatures = cosigner_sigs.len(),
        "sign_delta_proposal appended signature"
    );

    // Update the delta proposal with the new signature
    delta_proposal.status = DeltaStatus::Pending {
        timestamp,
        proposer_id,
        cosigner_sigs,
    };

    // Store the updated proposal
    resolved
        .backend
        .update_delta_proposal(&commitment, &delta_proposal)
        .await
        .map_err(PsmError::StorageError)?;

    Ok(SignDeltaProposalResult {
        delta: delta_proposal.clone(),
    })
}
