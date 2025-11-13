use std::collections::HashSet;

use miden_client::ClientError;
use miden_objects::account::Signature as AccountSignature;
use miden_objects::crypto::dsa::rpo_falcon512::Signature as RpoFalconSignature;
use miden_objects::transaction::TransactionSummary;
use private_state_manager_client::{verify_commitment_signature, FromJson, ToJson};
use private_state_manager_shared::hex::FromHex;

use crate::display::{print_info, print_section, print_success, print_waiting, shorten_hex};
use crate::helpers::{commitment_from_hex, format_word_as_hex};
use crate::multisig::{build_signature_advice_entry, build_update_signers_transaction_request};
use crate::proposals::{count_signatures, extract_proposal_metadata, get_signers};
use crate::state::SessionState;

use super::generate_keypair::pubkey_commitment_hex;

pub async fn action_finalize_pending_transaction(state: &mut SessionState) -> Result<(), String> {
    print_section("Finalize Proposal");

    state.configure_psm_auth()?;
    let finalize_auth_pubkey = {
        let psm_client = state.get_psm_client_mut()?;
        psm_client.auth_pubkey_hex().map_err(|e| e.to_string())?
    };
    let finalize_auth_commitment = pubkey_commitment_hex(&finalize_auth_pubkey);
    println!(
        "DEBUG: finalize auth signer pubkey {} (commitment {})",
        shorten_hex(&finalize_auth_pubkey),
        finalize_auth_commitment
            .as_ref()
            .map(|c| shorten_hex(c))
            .unwrap_or_else(|| "<invalid>".to_string())
    );
    println!(
        "DEBUG: state.cosigner_commitments before finalize {:?}",
        state.cosigner_commitments
    );
    let account = state.get_account()?;
    let account_id = account.id();

    print_waiting("Fetching pending proposals from PSM");
    let psm_client = state.get_psm_client_mut()?;
    let proposals_response = psm_client
        .get_delta_proposals(&account_id)
        .await
        .map_err(|e| format!("Failed to get proposals: {}", e))?;

    let proposals = &proposals_response.proposals;

    if proposals.is_empty() {
        print_info("No pending proposals found");
        return Err("No proposals to finalize".to_string());
    }

    println!("\nPending Proposals:");
    for (idx, proposal) in proposals.iter().enumerate() {
        let metadata = extract_proposal_metadata(proposal);
        let signature_count = count_signatures(proposal);
        let signers = get_signers(proposal);

        println!("  [{}] Proposal (nonce: {})", idx + 1, proposal.nonce);
        println!("      Type: {}", metadata.proposal_type);
        println!("      Signatures: {}", signature_count);

        if !signers.is_empty() {
            println!("      Signers:");
            for signer in &signers {
                println!("        - {}", shorten_hex(signer));
            }
        }
    }

    print!("\nSelect proposal number to finalize: ");
    std::io::Write::flush(&mut std::io::stdout()).map_err(|e| format!("Failed to flush: {}", e))?;
    let mut choice = String::new();
    std::io::stdin()
        .read_line(&mut choice)
        .map_err(|e| format!("Failed to read input: {}", e))?;

    let idx: usize = choice
        .trim()
        .parse()
        .map_err(|_| "Invalid proposal number".to_string())?;

    if idx == 0 || idx > proposals.len() {
        return Err("Invalid proposal number".to_string());
    }

    let proposal = &proposals[idx - 1];
    let metadata = extract_proposal_metadata(proposal);
    let signature_count = count_signatures(proposal);

    println!(
        "Metadata threshold={}, commitments={:?}",
        metadata.new_threshold.unwrap_or(0),
        metadata.signer_commitments_hex
    );

    print_info(&format!(
        "\nFinalizing proposal (nonce: {})",
        proposal.nonce
    ));
    print_info(&format!("Type: {}", metadata.proposal_type));
    print_info(&format!("Signatures collected: {}", signature_count));
    println!(
        "DEBUG: Required commitments for finalization: {:?}",
        metadata.signer_commitments_hex
    );

    let delta_payload_json = proposal.delta_payload.as_ref();
    let payload_wrapper: serde_json::Value = serde_json::from_str(delta_payload_json)
        .map_err(|e| format!("Failed to parse delta payload: {}", e))?;

    let tx_summary_value = payload_wrapper
        .get("tx_summary")
        .ok_or("Missing tx_summary in delta payload")?;

    let tx_summary = TransactionSummary::from_json(tx_summary_value)
        .map_err(|e| format!("Failed to deserialize transaction summary: {}", e))?;

    let tx_summary_commitment = tx_summary.to_commitment();
    let tx_summary_commitment_hex = format!("0x{}", hex::encode(tx_summary_commitment.as_bytes()));

    let mut signature_advice = Vec::new();
    let required_commitments: HashSet<String> =
        metadata.signer_commitments_hex.iter().cloned().collect();
    let mut added_signers: HashSet<String> = HashSet::new();

    print_info("Building signature advice from cosigner signatures");

    if let Some(ref status) = proposal.status {
        if let Some(ref status_oneof) = status.status {
            use private_state_manager_client::delta_status::Status;
            if let Status::Pending(ref pending) = status_oneof {
                println!("Pending cosigner signatures:");
                for (idx, cosigner_sig) in pending.cosigner_sigs.iter().enumerate() {
                    println!("  signer_id={}", shorten_hex(&cosigner_sig.signer_id));

                    let sig_json: serde_json::Value = serde_json::from_str(&cosigner_sig.signature)
                        .map_err(|e| format!("Failed to parse cosigner signature JSON: {}", e))?;

                    let sig_hex = sig_json
                        .get("signature")
                        .and_then(|v| v.as_str())
                        .ok_or("Missing signature field")?;

                    match verify_commitment_signature(
                        &tx_summary_commitment_hex,
                        &cosigner_sig.signer_id,
                        sig_hex,
                    ) {
                        Ok(true) => println!("    ✔ signature {} is valid", idx + 1),
                        Ok(false) => println!("    ✗ signature {} is INVALID", idx + 1),
                        Err(err) => {
                            println!("    ! verification error for signature {}: {err}", idx + 1)
                        }
                    }

                    if !required_commitments
                        .iter()
                        .any(|c| c.eq_ignore_ascii_case(&cosigner_sig.signer_id))
                    {
                        println!(
                            "    ! skipping signature {}: signer not part of expected commitments",
                            idx + 1
                        );
                        continue;
                    }

                    if !added_signers.insert(cosigner_sig.signer_id.clone()) {
                        println!(
                            "    ! skipping signature {}: signer already processed",
                            idx + 1
                        );
                        continue;
                    }

                    let sig = RpoFalconSignature::from_hex(sig_hex)
                        .map_err(|e| format!("Invalid cosigner signature: {}", e))?;

                    let commitment = commitment_from_hex(&cosigner_sig.signer_id)?;
                    signature_advice.push(build_signature_advice_entry(
                        commitment,
                        tx_summary_commitment,
                        &AccountSignature::from(sig),
                    ));
                    println!(
                        "DEBUG: Added cosigner {} to advice (total now {})",
                        shorten_hex(&cosigner_sig.signer_id),
                        signature_advice.len()
                    );
                }
            }
        }
    }

    print_success(&format!(
        "Built signature advice with {} cosigner signatures",
        signature_advice.len()
    ));
    for (idx, (key, _values)) in signature_advice.iter().enumerate() {
        println!(
            "DEBUG: Advice entry [{}] key {}",
            idx,
            format_word_as_hex(key)
        );
    }

    print_waiting("Pushing delta to PSM for acknowledgment");
    let push_response = {
        let psm_client = state.get_psm_client_mut()?;
        psm_client
            .push_delta(
                &account_id,
                proposal.nonce,
                proposal.prev_commitment.clone(),
                &tx_summary.to_json(),
            )
            .await
            .map_err(|e| format!("Failed to push delta to PSM: {}", e))?
    };

    if !push_response.success {
        return Err(format!("PSM rejected delta: {}", push_response.message));
    }

    let ack_sig = push_response
        .ack_sig
        .ok_or("PSM did not return acknowledgment signature")?;

    print_success("✓ Received PSM acknowledgment signature");

    let psm_commitment_hex = {
        let psm_client = state.get_psm_client_mut()?;
        psm_client
            .get_pubkey()
            .await
            .map_err(|e| format!("Failed to get PSM commitment: {}", e))?
    };

    let ack_sig_with_prefix = if ack_sig.starts_with("0x") {
        ack_sig.clone()
    } else {
        format!("0x{}", ack_sig)
    };

    let ack_signature = RpoFalconSignature::from_hex(&ack_sig_with_prefix)
        .map_err(|e| format!("Failed to parse PSM ack signature: {}", e))?;

    let psm_commitment = commitment_from_hex(&psm_commitment_hex)?;
    signature_advice.push(build_signature_advice_entry(
        psm_commitment,
        tx_summary_commitment,
        &AccountSignature::from(ack_signature),
    ));

    print_success(&format!(
        "Built complete signature advice with {} signatures (PSM + {} cosigners)",
        signature_advice.len(),
        signature_advice.len() - 1
    ));
    println!(
        "DEBUG: Final advice entry count (including PSM): {}",
        signature_advice.len()
    );

    print_waiting("Building final transaction request");

    let salt = metadata.salt();
    let signer_commitments = metadata.signer_commitments();
    let new_threshold = metadata
        .new_threshold
        .ok_or("Missing new_threshold in proposal metadata")?;

    let (final_tx_request, _final_config_hash) = build_update_signers_transaction_request(
        new_threshold,
        &signer_commitments,
        salt,
        signature_advice,
    )
    .map_err(|e| format!("Failed to build final transaction request: {}", e))?;

    print_waiting("Executing transaction locally");
    let tx_result = {
        let miden_client = state.get_miden_client_mut()?;
        match miden_client
            .new_transaction(account_id, final_tx_request)
            .await
        {
            Ok(result) => result,
            Err(ClientError::TransactionExecutorError(tx_err)) => {
                return Err(format!("Transaction execution failed:\n{tx_err}"));
            }
            Err(err) => return Err(format!("Transaction execution failed: {err}")),
        }
    };

    let new_nonce = tx_result.account_delta().nonce_delta().as_int();

    print_success(&format!(
        "✓ Transaction executed locally! New nonce: {}",
        new_nonce
    ));

    print_waiting("Proving and submitting transaction to Miden node");
    {
        let miden_client = state.get_miden_client_mut()?;
        miden_client
            .submit_transaction(tx_result.clone())
            .await
            .map_err(|e| format!("Failed to submit transaction: {}", e))?
    };

    print_success("✓ Transaction proven and submitted to Miden node!");

    print_info(&format!(
        "New configuration: {}-of-{}",
        new_threshold,
        metadata.signer_commitments_hex.len()
    ));

    print_waiting("Updating local account state");

    let current_account = state.get_account_mut()?;
    current_account
        .apply_delta(tx_result.account_delta())
        .map_err(|e| format!("Failed to apply account delta: {}", e))?;

    print_success("Local account state updated");

    Ok(())
}
