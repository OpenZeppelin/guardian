use crate::builder::state::AppState;
use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus, ProposalSignature};
use crate::error::{PsmError, Result};
use crate::metadata::auth::Credentials;
use crate::services::resolve_account;

#[derive(Debug, Clone)]
pub struct SignDeltaProposalParams {
    pub account_id: String,
    pub commitment: String,
    pub signature_scheme: String,
    pub signature: String,
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
        signature_scheme,
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
    let signer_id = match &credentials {
        Credentials::Signature { pubkey, .. } => pubkey.clone(),
    };

    // Check if already signed by this signer
    if cosigner_sigs.iter().any(|sig| sig.signer_id == signer_id) {
        return Err(PsmError::ProposalAlreadySigned { signer_id });
    }

    // Create the proposal signature based on scheme
    let proposal_signature = match signature_scheme.as_str() {
        "falcon" => ProposalSignature::Falcon { signature },
        _ => {
            return Err(PsmError::InvalidProposalSignature(format!(
                "Unknown signature scheme: {}",
                signature_scheme
            )));
        }
    };

    // Add the new signature
    let new_signature = CosignerSignature {
        signature: proposal_signature,
        timestamp: state.clock.now_rfc3339(),
        signer_id: signer_id.clone(),
    };
    cosigner_sigs.push(new_signature);

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
