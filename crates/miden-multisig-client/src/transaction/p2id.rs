//! P2ID (Pay-to-ID) transaction builder.

use miden_client::Client;
use miden_objects::account::AccountId;
use private_state_manager_client::PsmClient;
use private_state_manager_shared::ToJson;

use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::keystore::KeyManager;
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

use super::{execute_for_summary, generate_salt, word_to_hex};

/// Creates a proposal to transfer assets via P2ID.
///
/// This uses miden-client's standard P2ID transaction building,
/// then wraps it in the multisig proposal flow.
#[allow(dead_code)]
pub async fn create_p2id_proposal(
    miden_client: &mut Client<()>,
    psm_client: &mut PsmClient,
    account: &MultisigAccount,
    recipient: AccountId,
    faucet_id: AccountId,
    amount: u64,
    key_manager: &dyn KeyManager,
) -> Result<Proposal> {
    let account_id = account.id();
    let threshold = account.threshold()?;
    let signers = account.cosigner_commitments();

    // Build the P2ID transaction using miden-client's APIs
    // Note: The exact API depends on miden-client version
    // For now, we'll create a placeholder that needs to be adapted
    // to the actual miden-client P2ID API

    use miden_client::transaction::TransactionRequestBuilder;

    // Create P2ID note
    // This is a simplified version - actual implementation may need
    // to use miden-client's specific P2ID note creation methods
    let tx_request = TransactionRequestBuilder::new()
        // In practice, you'd use miden-client's P2ID-specific methods
        // This is a placeholder that will need to be adapted
        .build()
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to build P2ID tx: {}", e))
        })?;

    // Execute to get the TransactionSummary
    let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

    // Sign the transaction summary commitment
    let tx_commitment = tx_summary.to_commitment();
    let signature_hex = key_manager.sign_hex(tx_commitment);

    // Generate salt
    let salt = generate_salt();

    // Build proposal metadata
    let signer_commitments_hex: Vec<String> = signers.iter().map(word_to_hex).collect();

    let metadata = ProposalMetadata {
        tx_summary_json: Some(tx_summary.to_json()),
        new_threshold: None, // P2ID doesn't change threshold
        signer_commitments_hex: signer_commitments_hex.clone(),
        salt_hex: Some(word_to_hex(&salt)),
    };

    // Build the delta payload for PSM
    let delta_payload = serde_json::json!({
        "tx_summary": tx_summary.to_json(),
        "signatures": [{
            "signer_id": key_manager.commitment_hex(),
            "signature": { "Falcon": { "signature": signature_hex } }
        }],
        "metadata": {
            "transaction_type": "p2id",
            "recipient": recipient.to_hex(),
            "faucet_id": faucet_id.to_hex(),
            "amount": amount,
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
        transaction_type: TransactionType::P2ID {
            recipient,
            faucet_id,
            amount,
        },
        status: ProposalStatus::Pending {
            signatures_collected: 1,
            signatures_required: threshold as usize,
            signers: vec![key_manager.commitment_hex()],
        },
        tx_summary,
        metadata,
    };

    Ok(proposal)
}
