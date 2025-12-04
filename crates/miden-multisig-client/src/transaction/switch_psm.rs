//! Switch PSM server transaction builder.

use miden_client::transaction::{TransactionRequestBuilder, TransactionScript};
use miden_client::{Client, ScriptBuilder};
use miden_confidential_contracts::masm_builder::get_multisig_library;
use miden_objects::Word;
use private_state_manager_client::PsmClient;
use private_state_manager_shared::ToJson;

use crate::account::MultisigAccount;
use crate::error::{MultisigError, Result};
use crate::keystore::KeyManager;
use crate::proposal::{Proposal, ProposalMetadata, ProposalStatus, TransactionType};

use super::{execute_for_summary, generate_salt, word_to_hex};

/// Creates a proposal to switch PSM servers.
///
/// This updates:
/// 1. Storage slot 5: PSM public key commitment (on-chain)
/// 2. After finalization, the account should be registered with the new PSM
#[allow(dead_code)]
pub async fn create_switch_psm_proposal(
    miden_client: &mut Client<()>,
    psm_client: &mut PsmClient,
    account: &MultisigAccount,
    new_psm_endpoint: &str,
    new_psm_commitment: Word,
    key_manager: &dyn KeyManager,
) -> Result<Proposal> {
    let account_id = account.id();
    let threshold = account.threshold()?;
    let signers = account.cosigner_commitments();

    // Build the switch PSM transaction script
    // This script updates storage slot 5 with the new PSM commitment
    let tx_script = build_switch_psm_script()?;

    // Generate salt
    let salt = generate_salt();

    // Build the transaction request
    let tx_request = TransactionRequestBuilder::new()
        .custom_script(tx_script)
        .script_arg(new_psm_commitment)
        .auth_arg(salt)
        .build()
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to build switch PSM tx: {}", e))
        })?;

    // Execute to get the TransactionSummary
    let tx_summary = execute_for_summary(miden_client, account_id, tx_request).await?;

    // Sign the transaction summary commitment
    let tx_commitment = tx_summary.to_commitment();
    let signature_hex = key_manager.sign_hex(tx_commitment);

    // Build proposal metadata
    let signer_commitments_hex: Vec<String> = signers.iter().map(word_to_hex).collect();

    let metadata = ProposalMetadata {
        tx_summary_json: Some(tx_summary.to_json()),
        new_threshold: None, // Switch PSM doesn't change threshold
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
            "transaction_type": "switch_psm",
            "new_psm_endpoint": new_psm_endpoint,
            "new_psm_commitment": word_to_hex(&new_psm_commitment),
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
        transaction_type: TransactionType::SwitchPsm {
            new_endpoint: new_psm_endpoint.to_string(),
            new_commitment: new_psm_commitment,
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

/// Builds the switch PSM transaction script.
#[allow(dead_code)]
fn build_switch_psm_script() -> Result<TransactionScript> {
    let multisig_library = get_multisig_library().map_err(|e| {
        MultisigError::TransactionExecution(format!("failed to get multisig library: {}", e))
    })?;

    // Script to update PSM commitment in storage slot 5
    // Note: The actual MASM implementation depends on the PSM component's interface
    // This is a placeholder - actual implementation may need different procedure calls
    let tx_script_code = "
        begin
            # Update PSM public key in storage slot 5
            # The new commitment is passed as script_arg
            call.::update_psm_commitment
        end
    ";

    let tx_script = ScriptBuilder::new(true)
        .with_dynamically_linked_library(&multisig_library)
        .map_err(|e| MultisigError::TransactionExecution(format!("failed to link library: {}", e)))?
        .compile_tx_script(tx_script_code)
        .map_err(|e| {
            MultisigError::TransactionExecution(format!("failed to compile script: {}", e))
        })?;

    Ok(tx_script)
}
