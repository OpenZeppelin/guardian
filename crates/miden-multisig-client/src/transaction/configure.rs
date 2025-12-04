//! Add/Remove cosigner transaction builders.

use miden_client::Client;
use miden_objects::Word;
use private_state_manager_client::PsmClient;
use private_state_manager_shared::{DeltaSignature, ProposalSignature, ToJson};

use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::keystore::KeyManager;
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

use super::{
    build_update_signers_transaction_request, execute_for_summary, generate_salt, word_to_hex,
};

/// Creates a proposal to add a new cosigner.
///
/// This will:
/// 1. Build the update_signers transaction with the new signer added
/// 2. Execute locally to get the TransactionSummary
/// 3. Sign the summary with the user's key
/// 4. Push the proposal to PSM
pub async fn create_add_cosigner_proposal(
    miden_client: &mut Client<()>,
    psm_client: &mut PsmClient,
    account: &MultisigAccount,
    new_commitment: Word,
    key_manager: &dyn KeyManager,
) -> Result<Proposal> {
    let account_id = account.id();
    let current_threshold = account.threshold()?;
    let mut current_signers = account.cosigner_commitments();

    // Add the new signer
    current_signers.push(new_commitment);

    // Keep same threshold (could be configurable in the future)
    let new_threshold = current_threshold as u64;

    // Generate salt for replay protection
    let salt = generate_salt();

    // Build the transaction request (without signatures - we just want the summary)
    let (tx_request, _config_hash) = build_update_signers_transaction_request(
        new_threshold,
        &current_signers,
        salt,
        std::iter::empty(),
    )?;

    // Execute to get the TransactionSummary
    let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

    // Sign the transaction summary commitment
    let tx_commitment = tx_summary.to_commitment();
    let signature_hex = key_manager.sign_hex(tx_commitment);

    // Build proposal metadata
    let signer_commitments_hex: Vec<String> = current_signers.iter().map(word_to_hex).collect();

    let metadata = ProposalMetadata {
        tx_summary_json: Some(tx_summary.to_json()),
        new_threshold: Some(new_threshold),
        signer_commitments_hex: signer_commitments_hex.clone(),
        salt_hex: Some(word_to_hex(&salt)),
    };

    // Build signature using proper types for correct serialization
    let delta_signature = DeltaSignature {
        signer_id: key_manager.commitment_hex(),
        signature: ProposalSignature::Falcon {
            signature: signature_hex,
        },
    };

    // Build the delta payload for PSM
    let delta_payload = serde_json::json!({
        "tx_summary": tx_summary.to_json(),
        "signatures": [delta_signature],
        "metadata": {
            "new_threshold": new_threshold,
            "signer_commitments_hex": signer_commitments_hex,
            "salt_hex": word_to_hex(&salt),
        }
    });

    // Push proposal to PSM
    let nonce = account.nonce() + 1;
    let response = psm_client
        .push_delta_proposal(&account_id, nonce, &delta_payload)
        .await
        .map_err(|e| MultisigError::PsmServer(format!("failed to push proposal: {}", e)))?;

    // Build the Proposal
    let proposal = Proposal {
        id: response.commitment,
        nonce,
        transaction_type: TransactionType::AddCosigner { new_commitment },
        status: ProposalStatus::Pending {
            signatures_collected: 1,
            signatures_required: current_signers.len() - 1, // threshold of original signers
            signers: vec![key_manager.commitment_hex()],
        },
        tx_summary,
        metadata,
    };

    Ok(proposal)
}

/// Creates a proposal to remove a cosigner.
///
/// This will:
/// 1. Build the update_signers transaction with the signer removed
/// 2. Execute locally to get the TransactionSummary
/// 3. Sign the summary with the user's key
/// 4. Push the proposal to PSM
pub async fn create_remove_cosigner_proposal(
    miden_client: &mut Client<()>,
    psm_client: &mut PsmClient,
    account: &MultisigAccount,
    commitment_to_remove: Word,
    key_manager: &dyn KeyManager,
) -> Result<Proposal> {
    let account_id = account.id();
    let current_threshold = account.threshold()?;
    let current_signers = account.cosigner_commitments();

    // Remove the signer
    let new_signers: Vec<Word> = current_signers
        .iter()
        .filter(|&c| c != &commitment_to_remove)
        .copied()
        .collect();

    if new_signers.len() == current_signers.len() {
        return Err(MultisigError::InvalidConfig(
            "commitment to remove not found in signers".to_string(),
        ));
    }

    // Adjust threshold if needed (can't be more than signers)
    let new_threshold = std::cmp::min(current_threshold as u64, new_signers.len() as u64);

    if new_signers.is_empty() {
        return Err(MultisigError::InvalidConfig(
            "cannot remove last signer".to_string(),
        ));
    }

    // Generate salt for replay protection
    let salt = generate_salt();

    // Build the transaction request
    let (tx_request, _config_hash) = build_update_signers_transaction_request(
        new_threshold,
        &new_signers,
        salt,
        std::iter::empty(),
    )?;

    // Execute to get the TransactionSummary
    let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

    // Sign the transaction summary commitment
    let tx_commitment = tx_summary.to_commitment();
    let signature_hex = key_manager.sign_hex(tx_commitment);

    // Build proposal metadata
    let signer_commitments_hex: Vec<String> = new_signers.iter().map(word_to_hex).collect();

    let metadata = ProposalMetadata {
        tx_summary_json: Some(tx_summary.to_json()),
        new_threshold: Some(new_threshold),
        signer_commitments_hex: signer_commitments_hex.clone(),
        salt_hex: Some(word_to_hex(&salt)),
    };

    // Build signature using proper types for correct serialization
    let delta_signature = DeltaSignature {
        signer_id: key_manager.commitment_hex(),
        signature: ProposalSignature::Falcon {
            signature: signature_hex,
        },
    };

    // Build the delta payload for PSM
    let delta_payload = serde_json::json!({
        "tx_summary": tx_summary.to_json(),
        "signatures": [delta_signature],
        "metadata": {
            "new_threshold": new_threshold,
            "signer_commitments_hex": signer_commitments_hex,
            "salt_hex": word_to_hex(&salt),
        }
    });

    // Push proposal to PSM
    let nonce = account.nonce() + 1;
    let response = psm_client
        .push_delta_proposal(&account_id, nonce, &delta_payload)
        .await
        .map_err(|e| MultisigError::PsmServer(format!("failed to push proposal: {}", e)))?;

    // Build the Proposal
    let proposal = Proposal {
        id: response.commitment,
        nonce,
        transaction_type: TransactionType::RemoveCosigner {
            commitment: commitment_to_remove,
        },
        status: ProposalStatus::Pending {
            signatures_collected: 1,
            signatures_required: current_threshold as usize,
            signers: vec![key_manager.commitment_hex()],
        },
        tx_summary,
        metadata,
    };

    Ok(proposal)
}
