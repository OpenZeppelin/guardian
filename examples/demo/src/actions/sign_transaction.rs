use miden_client::Serializable;
use rustyline::DefaultEditor;

use crate::display::{print_info, print_section, print_success, print_waiting, shorten_hex};
use crate::helpers::commitment_from_hex;
use crate::menu::prompt_input;
use crate::proposals::{
    count_signatures, extract_proposal_metadata, has_signer_signed, ProposalMetadata,
};
use crate::state::SessionState;

use super::generate_keypair::pubkey_commitment_hex;

pub async fn action_sign_transaction(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Sign a Proposal");

    let account = state.get_account()?;
    let account_id = account.id();

    print_waiting("Configuring PSM authentication for current signer");
    state.configure_psm_auth()?;
    print_success("Configured PSM auth with current signer key");
    let sign_auth_pubkey_hex = {
        let psm_client = state.get_psm_client_mut()?;
        psm_client.auth_pubkey_hex().map_err(|e| e.to_string())?
    };
    let sign_auth_commitment_hex = pubkey_commitment_hex(&sign_auth_pubkey_hex);
    println!(
        "DEBUG: sign_delta_proposal auth signer pubkey {} (commitment {})",
        shorten_hex(&sign_auth_pubkey_hex),
        sign_auth_commitment_hex
            .as_ref()
            .map(|c| shorten_hex(c))
            .unwrap_or_else(|| "<invalid>".to_string())
    );
    println!(
        "DEBUG: state.cosigner_commitments {:?}",
        state.cosigner_commitments
    );

    let user_secret_key = state.get_secret_key()?.clone();
    let user_commitment_hex = state.get_commitment_hex()?.to_string();

    print_waiting("Fetching proposals from PSM server");
    let psm_client = state.get_psm_client_mut()?;
    let proposals_response = psm_client
        .get_delta_proposals(&account_id)
        .await
        .map_err(|e| format!("Failed to fetch proposals: {}", e))?;

    let proposals = proposals_response.proposals;
    if proposals.is_empty() {
        print_info("No pending proposals found for this account");
        return Ok(());
    }

    print_info(&format!("\nFound {} pending proposal(s):", proposals.len()));
    println!();

    let mut proposal_info: Vec<(ProposalMetadata, Option<String>)> = Vec::new();

    for (idx, proposal) in proposals.iter().enumerate() {
        let metadata = extract_proposal_metadata(proposal);
        let tx_commitment = metadata.get_tx_commitment();
        let signature_count = count_signatures(proposal);

        println!("  [{}] Proposal (nonce: {})", idx + 1, proposal.nonce);
        println!("      Type: {}", metadata.proposal_type);
        println!(
            "      Signatures: {}/{}",
            signature_count,
            metadata.signers_required_hex.len()
        );

        if let Some(ref comm) = tx_commitment {
            println!("      TX Commitment: {}", shorten_hex(comm));
        }

        println!();
        proposal_info.push((metadata, tx_commitment));
    }

    let selection = prompt_input(editor, "Select proposal to sign (number): ")?;
    let idx = selection
        .parse::<usize>()
        .map_err(|_| "Invalid selection".to_string())?
        .checked_sub(1)
        .ok_or("Invalid selection (use 1-based index)".to_string())?;

    if idx >= proposals.len() {
        return Err("Selection out of range".to_string());
    }

    let selected_proposal = &proposals[idx];
    let (metadata, tx_commitment) = &proposal_info[idx];
    let tx_commitment = tx_commitment
        .as_ref()
        .ok_or("Could not extract transaction commitment from proposal".to_string())?;

    if !metadata
        .signer_commitments_hex
        .iter()
        .any(|c| c.eq_ignore_ascii_case(&user_commitment_hex))
    {
        println!(
            "DEBUG: Attempted to sign with {}, but expected commitments are {:?}",
            user_commitment_hex, metadata.signer_commitments_hex
        );
        return Err(format!(
            "Your key ({}) is not part of this proposal's signer set",
            shorten_hex(&user_commitment_hex)
        ));
    }

    println!(
        "DEBUG: Signing proposal as {}",
        shorten_hex(&user_commitment_hex)
    );
    println!(
        "DEBUG: Proposal expects signers: {:?}",
        metadata.signer_commitments_hex
    );

    if has_signer_signed(selected_proposal, &user_commitment_hex) {
        print_info(&format!(
            "You have already signed this proposal (commitment: {})",
            shorten_hex(&user_commitment_hex)
        ));
        return Ok(());
    }

    let proposal_id = tx_commitment.clone();

    print_info(&format!("\nProposal ID: {}", shorten_hex(&proposal_id)));
    print_waiting("Signing proposal with your key");
    println!(
        "DEBUG: Attempting to sign commitment {}",
        shorten_hex(&proposal_id)
    );

    let commitment_word = commitment_from_hex(&proposal_id)?;
    let user_signature_raw = user_secret_key.sign(commitment_word);
    let user_signature_hex = format!("0x{}", hex::encode(user_signature_raw.to_bytes()));
    println!(
        "DEBUG: Signing request body with signature {}",
        shorten_hex(&user_signature_hex)
    );

    let sign_response = psm_client
        .sign_delta_proposal(&account_id, &proposal_id, "falcon", &user_signature_hex)
        .await
        .map_err(|e| format!("Failed to sign proposal: {}", e))?;

    if !sign_response.success {
        return Err(format!(
            "Failed to sign proposal: {}",
            sign_response.message
        ));
    }

    print_success(&format!(
        "Successfully signed proposal with your key ({})",
        shorten_hex(&user_commitment_hex)
    ));

    if let Some(updated_delta) = sign_response.delta {
        println!(
            "DEBUG: Server returned signer list {:?}",
            extract_proposal_metadata(&updated_delta).signer_commitments_hex
        );
        let updated_metadata = extract_proposal_metadata(&updated_delta);
        let updated_sig_count = count_signatures(&updated_delta);

        print_info(&format!(
            "Signatures collected: {}/{}",
            updated_sig_count,
            updated_metadata.signers_required_hex.len()
        ));

        if updated_metadata.is_ready(updated_sig_count) {
            print_success("\n✓ All signatures collected!");
            print_info("This proposal can now be finalized using option [9]");
        } else {
            print_info("\nWaiting for more signatures...");
        }
    }

    Ok(())
}
