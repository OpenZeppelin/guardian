use miden_client::ClientError;
use miden_objects::utils::Serializable;
use miden_objects::{Felt, Word};
use private_state_manager_shared::{DeltaPayload, DeltaSignature, ToJson};
use rustyline::DefaultEditor;

use crate::account_inspector::AccountInspector;
use crate::display::{
    print_full_hex, print_info, print_section, print_success, print_waiting, shorten_hex,
};
use crate::helpers::{commitment_from_hex, format_word_as_hex};
use crate::menu::prompt_input;
use crate::multisig::build_update_signers_transaction_request;
use crate::state::SessionState;

pub async fn action_add_cosigner(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Add Cosigner (Update to N+1)");

    let account = state.get_account()?;
    let account_id = account.id();
    let current_nonce = account.nonce().as_int();

    let _prev_commitment = format!("0x{}", hex::encode(account.commitment().as_bytes()));

    print_info("Enter the new cosigner's commitment:");
    let new_cosigner_commitment_hex = prompt_input(editor, "  New cosigner commitment: ")?;

    let commitment_stripped = new_cosigner_commitment_hex
        .strip_prefix("0x")
        .unwrap_or(&new_cosigner_commitment_hex);
    if commitment_stripped.len() != 64 {
        return Err(format!(
            "Invalid commitment length: expected 64 hex chars, got {}",
            commitment_stripped.len()
        ));
    }

    let new_cosigner_commitment_hex = if new_cosigner_commitment_hex.starts_with("0x") {
        new_cosigner_commitment_hex
    } else {
        format!("0x{}", new_cosigner_commitment_hex)
    };

    let new_cosigner_commitment = commitment_from_hex(&new_cosigner_commitment_hex)?;

    let storage = account.storage();
    let config_word = storage
        .get_item(0)
        .map_err(|e| format!("Failed to get multisig config: {}", e))?;

    let current_threshold = config_word[0].as_int();
    let current_num_cosigners = config_word[1].as_int();

    print_info(&format!(
        "Current config: {}-of-{}",
        current_threshold, current_num_cosigners
    ));
    print_info(&format!(
        "New config will be: {}-of-{}",
        current_threshold,
        current_num_cosigners + 1
    ));

    let inspector = AccountInspector::new(account);
    let existing_commitments_hex = inspector.extract_cosigner_commitments();

    if existing_commitments_hex.len() != current_num_cosigners as usize {
        return Err(format!(
            "Extracted commitments mismatch: found {}, expected {}",
            existing_commitments_hex.len(),
            current_num_cosigners
        ));
    }

    let mut signer_commitments: Vec<Word> = existing_commitments_hex
        .iter()
        .map(|hex| commitment_from_hex(hex))
        .collect::<Result<Vec<_>, _>>()?;

    signer_commitments.push(new_cosigner_commitment);

    state.cosigner_commitments = existing_commitments_hex.clone();
    state
        .cosigner_commitments
        .push(new_cosigner_commitment_hex.clone());

    let salt = Word::from([
        Felt::new(rand::random()),
        Felt::new(0),
        Felt::new(0),
        Felt::new(0),
    ]);

    let (tx_request, _config_hash) = build_update_signers_transaction_request(
        current_threshold,
        &signer_commitments,
        salt,
        vec![],
    )
    .map_err(|e| format!("Failed to build transaction request: {}", e))?;

    print_waiting("Creating proposal");

    let miden_client = state.get_miden_client_mut()?;

    miden_client
        .sync_state()
        .await
        .map_err(|e| format!("Failed to sync client state: {}", e))?;

    let tx_summary = match miden_client
        .new_transaction(account_id, tx_request.clone())
        .await
    {
        Err(ClientError::TransactionExecutorError(
            miden_client::transaction::TransactionExecutorError::Unauthorized(summary),
        )) => summary,
        Ok(_) => {
            return Err("Expected Unauthorized error but transaction succeeded".to_string());
        }
        Err(e) => {
            return Err(format!("Simulation failed: {}", e));
        }
    };

    let user_secret_key = state.get_secret_key()?.clone();
    let user_commitment_hex = state.get_commitment_hex()?.to_string();

    let tx_summary_commitment = tx_summary.to_commitment();
    let proposal_commitment = format!("0x{}", hex::encode(tx_summary_commitment.as_bytes()));

    let user_signature_raw = user_secret_key.sign(tx_summary_commitment);
    let user_signature_hex = format!("0x{}", hex::encode(user_signature_raw.to_bytes()));

    let salt_hex = format_word_as_hex(&salt);
    let signer_commitments_hex: Vec<String> =
        signer_commitments.iter().map(format_word_as_hex).collect();

    let mut delta_payload_value = DeltaPayload::new(tx_summary.to_json())
        .with_signature(DeltaSignature {
            signer_id: user_commitment_hex.clone(),
            signature: user_signature_hex,
            signature_scheme: "falcon".to_string(),
        })
        .to_json();

    if let Some(obj) = delta_payload_value.as_object_mut() {
        obj.insert(
            "metadata".to_string(),
            serde_json::json!({
                "new_threshold": current_threshold,
                "signer_commitments_hex": signer_commitments_hex,
                "salt_hex": salt_hex,
            }),
        );
    }

    let delta_payload = delta_payload_value;

    let psm_client = state.get_psm_client_mut()?;
    let proposal_response = psm_client
        .push_delta_proposal(&account_id, current_nonce, &delta_payload)
        .await
        .map_err(|e| format!("Failed to push proposal to PSM: {}", e))?;

    if !proposal_response.success {
        return Err(format!(
            "Failed to create proposal: {}",
            proposal_response.message
        ));
    }

    print_success("Proposal created on PSM server");
    print_full_hex("\nProposal ID (Commitment)", &proposal_commitment);
    print_success(&format!(
        "Automatically signed with your key ({})",
        shorten_hex(&user_commitment_hex)
    ));

    print_info(&format!(
        "\nSignatures collected: 1/{}",
        current_num_cosigners
    ));
    print_info("\nNext steps:");
    print_info("  1. Share the Proposal ID above with other cosigners");
    print_info("  2. Other cosigners use option [8] 'Sign a proposal'");
    print_info(&format!(
        "  3. Once you have {}/{} signatures, use option [9] 'Finalize a proposal'",
        current_num_cosigners, current_num_cosigners
    ));

    Ok(())
}
