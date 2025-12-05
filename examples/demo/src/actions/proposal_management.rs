//! Unified proposal management - all proposal operations in one place.

use std::path::Path;

use miden_multisig_client::{
    commitment_from_hex, ensure_hex_prefix, Asset, ExportedProposal, NoteId, ProposalStatus,
    TransactionType,
};
use miden_objects::account::AccountId;
use rustyline::DefaultEditor;

use crate::display::{
    print_error, print_full_hex, print_info, print_section, print_success, print_waiting,
    shorten_hex,
};
use crate::menu::prompt_input;
use crate::state::SessionState;

/// Proposal Management submenu - all proposal operations.
pub async fn action_proposal_management(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    loop {
        print_proposal_menu();

        let choice = prompt_input(editor, "Choice: ")?;

        match choice.as_str() {
            "1" => {
                if let Err(e) = action_create_proposal(state, editor).await {
                    print_error(&e);
                }
            }
            "2" => {
                if let Err(e) = action_view_proposals(state).await {
                    print_error(&e);
                }
            }
            "3" => {
                if let Err(e) = action_sign_proposal(state, editor).await {
                    print_error(&e);
                }
            }
            "4" => {
                if let Err(e) = action_execute_proposal(state, editor).await {
                    print_error(&e);
                }
            }
            "5" => {
                if let Err(e) = action_export_proposal(state, editor).await {
                    print_error(&e);
                }
            }
            "6" => {
                if let Err(e) = action_import_and_work(state, editor).await {
                    print_error(&e);
                }
            }
            "b" | "back" => return Ok(()),
            _ => print_error("Invalid choice"),
        }
    }
}

fn print_proposal_menu() {
    println!("\n┌─────────────────────────────────────────────┐");
    println!("│ Proposal Management                         │");
    println!("└─────────────────────────────────────────────┘");
    println!("  PSM Operations:");
    println!("  [1] Create proposal (via PSM)");
    println!("  [2] View pending proposals");
    println!("  [3] Sign a proposal");
    println!("  [4] Execute a proposal");
    println!();
    println!("  Offline/Export Operations:");
    println!("  [5] Export proposal to file");
    println!("  [6] Import & work with proposal file");
    println!();
    println!("  [b] Back to main menu");
    println!();
}

// =============================================================================
// PSM Operations
// =============================================================================

/// Create a proposal via PSM.
async fn action_create_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Create Proposal");

    let client = state.get_client()?;
    let account = client
        .account()
        .ok_or_else(|| "No account loaded".to_string())?;
    let threshold = account.threshold().map_err(|e| e.to_string())?;
    let current_num_cosigners = account.cosigner_commitments().len();

    print_info(&format!(
        "Current config: {}-of-{}",
        threshold, current_num_cosigners
    ));

    println!();
    println!("  Select proposal type:");
    println!("    [1] Add cosigner");
    println!("    [2] Remove cosigner");
    println!("    [3] Transfer assets (P2ID)");
    println!("    [4] Consume notes");
    println!("    [5] Switch PSM provider");
    println!("    [b] Back");
    println!();

    let choice = prompt_input(editor, "Choice: ")?;

    let transaction_type = match choice.as_str() {
        "1" => prompt_add_cosigner(editor)?,
        "2" => prompt_remove_cosigner(state, editor)?,
        "3" => prompt_p2id(editor)?,
        "4" => prompt_consume_notes(state, editor).await?,
        "5" => prompt_switch_psm(state, editor)?,
        "b" | "B" => return Ok(()),
        _ => return Err("Invalid choice".to_string()),
    };

    print_waiting("Creating proposal on PSM");

    let client = state.get_client_mut()?;
    let result = client.propose_transaction(transaction_type.clone()).await;

    match result {
        Ok(proposal) => {
            print_success("Proposal created on PSM server");
            print_full_hex("Proposal ID", &proposal.id);
            print_success(&format!(
                "Automatically signed with your key ({})",
                shorten_hex(&client.user_commitment_hex())
            ));
            print_info(&format!(
                "\nNeed {} signatures total. Use [3] to sign, [4] to execute.",
                threshold
            ));
            Ok(())
        }
        Err(e) => {
            print_error(&format!("PSM proposal creation failed: {}", e));
            print_info("\nWould you like to create this proposal offline instead? [y/N]");
            let fallback = prompt_input(editor, "Choice: ")?;

            if fallback.to_lowercase() == "y" {
                create_proposal_offline(state, editor, transaction_type).await
            } else {
                Err(format!("Proposal creation failed: {}", e))
            }
        }
    }
}

/// View pending proposals from PSM.
async fn action_view_proposals(state: &mut SessionState) -> Result<(), String> {
    print_section("View Pending Proposals");

    let client = state.get_client_mut()?;

    print_waiting("Fetching proposals from PSM");
    let proposals = client
        .list_proposals()
        .await
        .map_err(|e| format!("Failed to fetch proposals: {}", e))?;

    if proposals.is_empty() {
        print_info("No pending proposals found for this account");
        return Ok(());
    }

    print_info(&format!("\nFound {} pending proposal(s):", proposals.len()));
    println!();

    for (idx, proposal) in proposals.iter().enumerate() {
        let (collected, required) = proposal.signature_counts();

        println!("  [{}] Proposal", idx + 1);
        println!("      Type: {:?}", proposal.transaction_type);
        print_full_hex("      Proposal ID", &proposal.id);
        println!("      Signatures: {}/{}", collected, required);

        if let ProposalStatus::Pending { signers, .. } = &proposal.status {
            if !signers.is_empty() {
                println!("      Signers:");
                for signer in signers {
                    println!("        - {}", shorten_hex(signer));
                }
            }
        }
        println!();
    }

    Ok(())
}

/// Sign a proposal via PSM.
async fn action_sign_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Sign a Proposal");

    let client = state.get_client_mut()?;

    print_waiting("Fetching proposals from PSM");
    let proposals = client
        .list_proposals()
        .await
        .map_err(|e| format!("Failed to fetch proposals: {}", e))?;

    if proposals.is_empty() {
        print_info("No pending proposals found");
        return Ok(());
    }

    println!("\nPending Proposals:");
    for (idx, proposal) in proposals.iter().enumerate() {
        let (collected, required) = proposal.signature_counts();

        println!("  [{}] {}", idx + 1, shorten_hex(&proposal.id));
        println!("      Type: {:?}", proposal.transaction_type);
        println!("      Signatures: {}/{}", collected, required);
    }

    let selection = prompt_input(editor, "\nSelect proposal to sign (number): ")?;
    let idx = selection
        .parse::<usize>()
        .map_err(|_| "Invalid selection".to_string())?
        .checked_sub(1)
        .ok_or("Invalid selection".to_string())?;

    if idx >= proposals.len() {
        return Err("Selection out of range".to_string());
    }

    let proposal_id = proposals[idx].id.clone();

    print_waiting("Signing proposal");

    let client = state.get_client_mut()?;
    let updated = client
        .sign_proposal(&proposal_id)
        .await
        .map_err(|e| format!("Failed to sign: {}", e))?;

    print_success(&format!(
        "Signed with key {}",
        shorten_hex(&client.user_commitment_hex())
    ));

    let (collected, required) = updated.signature_counts();
    print_info(&format!("Signatures: {}/{}", collected, required));

    if updated.signatures_needed() == 0 {
        print_success("All signatures collected! Ready to execute with [4].");
    }

    Ok(())
}

/// Execute a proposal via PSM.
async fn action_execute_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Execute Proposal");

    let client = state.get_client_mut()?;

    print_waiting("Fetching proposals from PSM");
    let proposals = client
        .list_proposals()
        .await
        .map_err(|e| format!("Failed to get proposals: {}", e))?;

    if proposals.is_empty() {
        print_info("No pending proposals found");
        return Ok(());
    }

    println!("\nPending Proposals:");
    for (idx, proposal) in proposals.iter().enumerate() {
        let (collected, required) = proposal.signature_counts();
        let ready = if proposal.signatures_needed() == 0 {
            " ✓ READY"
        } else {
            ""
        };

        println!("  [{}] {}{}", idx + 1, shorten_hex(&proposal.id), ready);
        println!("      Type: {:?}", proposal.transaction_type);
        println!("      Signatures: {}/{}", collected, required);
    }

    let selection = prompt_input(editor, "\nSelect proposal to execute (number): ")?;
    let idx: usize = selection
        .trim()
        .parse()
        .map_err(|_| "Invalid selection".to_string())?;

    if idx == 0 || idx > proposals.len() {
        return Err("Invalid selection".to_string());
    }

    let proposal_id = proposals[idx - 1].id.clone();

    print_waiting("Executing proposal");

    let client = state.get_client_mut()?;
    client
        .execute_proposal(&proposal_id)
        .await
        .map_err(|e| format!("Failed to execute: {}", e))?;

    print_success("Transaction executed successfully!");

    let account = client
        .account()
        .ok_or_else(|| "No account loaded".to_string())?;
    print_success(&format!("Account updated. New nonce: {}", account.nonce()));

    Ok(())
}

// =============================================================================
// Offline/Export Operations
// =============================================================================

/// Export a proposal from PSM to file.
async fn action_export_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Export Proposal to File");

    let client = state.get_client_mut()?;

    print_waiting("Fetching proposals from PSM");
    let proposals = client
        .list_proposals()
        .await
        .map_err(|e| format!("Failed to get proposals: {}", e))?;

    if proposals.is_empty() {
        print_info("No pending proposals found");
        return Ok(());
    }

    println!("\nPending Proposals:");
    for (idx, proposal) in proposals.iter().enumerate() {
        let (collected, required) = proposal.signature_counts();

        println!("  [{}] {}", idx + 1, shorten_hex(&proposal.id));
        println!("      Type: {:?}", proposal.transaction_type);
        println!("      Signatures: {}/{}", collected, required);
    }

    let choice = prompt_input(editor, "\nSelect proposal to export (number): ")?;
    let idx: usize = choice.trim().parse().map_err(|_| "Invalid selection")?;

    if idx == 0 || idx > proposals.len() {
        return Err("Invalid selection".to_string());
    }

    let proposal_id = proposals[idx - 1].id.clone();

    let default_path = format!(
        "proposal_{}.json",
        shorten_hex(&proposal_id).replace("...", "_")
    );
    let path_input = prompt_input(editor, &format!("File path [{}]: ", default_path))?;
    let path = if path_input.is_empty() {
        default_path
    } else {
        path_input
    };

    print_waiting("Exporting proposal");

    let client = state.get_client_mut()?;
    client
        .export_proposal(&proposal_id, Path::new(&path))
        .await
        .map_err(|e| format!("Failed to export: {}", e))?;

    print_success(&format!("Proposal exported to: {}", path));
    print_info("Share this file with other cosigners for offline signing");

    Ok(())
}

/// Import a proposal file and work with it (sign/execute).
async fn action_import_and_work(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Import & Work with Proposal File");

    // Check if we already have an imported proposal
    if let Some(existing) = state.get_imported_proposal() {
        print_info(&format!(
            "Currently loaded proposal: {}",
            shorten_hex(&existing.id)
        ));
        print_info(&format!(
            "Signatures: {}/{}",
            existing.signatures_collected(),
            existing.signatures_required
        ));

        println!("\n  [1] Work with current proposal");
        println!("  [2] Import a different proposal");
        println!("  [b] Back");

        let choice = prompt_input(editor, "\nChoice: ")?;
        match choice.as_str() {
            "1" => return work_with_imported(state, editor).await,
            "2" => { /* continue to import */ }
            "b" => return Ok(()),
            _ => return Err("Invalid choice".to_string()),
        }
    }

    let path = prompt_input(editor, "File path: ")?;
    if path.is_empty() {
        return Err("File path is required".to_string());
    }

    print_waiting("Importing proposal");

    let client = state.get_client()?;
    let proposal = client
        .import_proposal(Path::new(&path))
        .map_err(|e| format!("Failed to import: {}", e))?;

    print_success("Proposal imported successfully!");
    print_proposal_details(&proposal);

    state.set_imported_proposal(proposal);

    work_with_imported(state, editor).await
}

/// Work with an imported proposal (sign or execute).
async fn work_with_imported(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    let proposal = state
        .get_imported_proposal()
        .ok_or_else(|| "No imported proposal".to_string())?
        .clone();

    println!("\n  [1] Sign this proposal");
    println!("  [2] Execute this proposal");
    println!("  [3] Save proposal to file");
    println!("  [b] Back");

    let choice = prompt_input(editor, "\nChoice: ")?;

    match choice.as_str() {
        "1" => sign_imported_proposal(state, editor).await,
        "2" => execute_imported_proposal(state, editor).await,
        "3" => save_imported_proposal(state, editor, &proposal),
        "b" => Ok(()),
        _ => Err("Invalid choice".to_string()),
    }
}

/// Sign an imported proposal.
async fn sign_imported_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    let mut proposal = state
        .take_imported_proposal()
        .ok_or_else(|| "No imported proposal".to_string())?;

    print_proposal_details(&proposal);

    let confirm = prompt_input(editor, "\nSign this proposal? [y/N]: ")?;
    if confirm.to_lowercase() != "y" {
        state.set_imported_proposal(proposal);
        return Err("Signing cancelled".to_string());
    }

    print_waiting("Signing proposal");

    let client = state.get_client()?;
    client
        .sign_imported_proposal(&mut proposal)
        .map_err(|e| format!("Failed to sign: {}", e))?;

    print_success("Proposal signed!");
    println!(
        "  Signatures: {}/{}",
        proposal.signatures_collected(),
        proposal.signatures_required
    );

    // Offer to save
    let save = prompt_input(editor, "\nSave signed proposal to file? [Y/n]: ")?;
    if save.to_lowercase() != "n" {
        let default_path = format!(
            "proposal_{}_signed.json",
            shorten_hex(&proposal.id).replace("...", "_")
        );
        let path = prompt_input(editor, &format!("File path [{}]: ", default_path))?;
        let path = if path.is_empty() { default_path } else { path };

        let json = proposal
            .to_json()
            .map_err(|e| format!("Serialize error: {}", e))?;
        std::fs::write(&path, json).map_err(|e| format!("Write error: {}", e))?;
        print_success(&format!("Saved to: {}", path));
    }

    state.set_imported_proposal(proposal);
    Ok(())
}

/// Execute an imported proposal.
async fn execute_imported_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    let proposal = state
        .get_imported_proposal()
        .ok_or_else(|| "No imported proposal".to_string())?
        .clone();

    print_proposal_details(&proposal);

    if !proposal.is_ready() {
        return Err(format!(
            "Not ready: need {} signatures, have {}",
            proposal.signatures_required,
            proposal.signatures_collected()
        ));
    }

    let confirm = prompt_input(editor, "\nExecute this proposal? [y/N]: ")?;
    if confirm.to_lowercase() != "y" {
        return Err("Execution cancelled".to_string());
    }

    print_waiting("Executing proposal");

    // Execute and get nonce in a scope to release borrow
    let nonce = {
        let client = state.get_client_mut()?;
        client
            .execute_imported_proposal(&proposal)
            .await
            .map_err(|e| format!("Failed to execute: {}", e))?;

        print_success("Transaction executed successfully!");

        client
            .account()
            .ok_or_else(|| "No account loaded".to_string())?
            .nonce()
    };

    // Clear imported proposal
    state.take_imported_proposal();

    print_success(&format!("Account updated. New nonce: {}", nonce));

    Ok(())
}

/// Save imported proposal to file.
fn save_imported_proposal(
    _state: &SessionState,
    editor: &mut DefaultEditor,
    proposal: &ExportedProposal,
) -> Result<(), String> {
    let default_path = format!(
        "proposal_{}.json",
        shorten_hex(&proposal.id).replace("...", "_")
    );
    let path = prompt_input(editor, &format!("File path [{}]: ", default_path))?;
    let path = if path.is_empty() { default_path } else { path };

    let json = proposal
        .to_json()
        .map_err(|e| format!("Serialize error: {}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("Write error: {}", e))?;

    print_success(&format!("Proposal saved to: {}", path));
    Ok(())
}

/// Create a proposal offline (fallback when PSM fails).
async fn create_proposal_offline(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
    transaction_type: TransactionType,
) -> Result<(), String> {
    print_section("Creating Proposal Offline");

    print_waiting("Creating proposal locally");

    let client = state.get_client_mut()?;
    let proposal = client
        .create_proposal_offline(transaction_type)
        .await
        .map_err(|e| format!("Failed to create offline proposal: {}", e))?;

    print_success("Proposal created offline!");
    print_proposal_details(&proposal);

    // Ask to save
    let default_path = format!(
        "proposal_{}.json",
        shorten_hex(&proposal.id).replace("...", "_")
    );
    let path = prompt_input(editor, &format!("\nSave to file [{}]: ", default_path))?;
    let path = if path.is_empty() { default_path } else { path };

    let json = proposal
        .to_json()
        .map_err(|e| format!("Serialize error: {}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("Write error: {}", e))?;

    print_success(&format!("Proposal saved to: {}", path));
    print_info("Share this file with other cosigners for signing");

    state.set_imported_proposal(proposal);

    Ok(())
}

// =============================================================================
// Helpers - Prompt for transaction type details
// =============================================================================

fn prompt_add_cosigner(editor: &mut DefaultEditor) -> Result<TransactionType, String> {
    print_info("Enter the new cosigner's commitment:");
    let hex = prompt_input(editor, "  New cosigner commitment: ")?;

    if hex.is_empty() {
        return Err("Commitment is required".to_string());
    }

    let new_commitment = commitment_from_hex(&ensure_hex_prefix(&hex))
        .map_err(|e| format!("Invalid commitment: {}", e))?;

    Ok(TransactionType::add_cosigner(new_commitment))
}

fn prompt_remove_cosigner(
    state: &SessionState,
    editor: &mut DefaultEditor,
) -> Result<TransactionType, String> {
    let client = state.get_client()?;
    let account = client
        .account()
        .ok_or_else(|| "No account loaded".to_string())?;

    let cosigners = account.cosigner_commitments();
    if cosigners.len() <= 1 {
        return Err("Cannot remove: only one cosigner remaining".to_string());
    }

    println!("\nCurrent cosigners:");
    for (i, commitment) in account.cosigner_commitments_hex().iter().enumerate() {
        println!("  [{}] {}", i + 1, shorten_hex(commitment));
    }

    let idx_str = prompt_input(editor, "\nSelect cosigner to remove: ")?;
    let idx: usize = idx_str.trim().parse().map_err(|_| "Invalid selection")?;

    if idx == 0 || idx > cosigners.len() {
        return Err("Invalid selection".to_string());
    }

    let commitment = cosigners[idx - 1];
    Ok(TransactionType::remove_cosigner(commitment))
}

fn prompt_p2id(editor: &mut DefaultEditor) -> Result<TransactionType, String> {
    print_info("Enter the recipient account ID:");
    let recipient_hex = prompt_input(editor, "  Recipient account ID: ")?;
    let recipient =
        AccountId::from_hex(&recipient_hex).map_err(|e| format!("Invalid recipient: {}", e))?;

    print_info("Enter the faucet/asset ID:");
    let faucet_hex = prompt_input(editor, "  Faucet ID: ")?;
    let faucet_id =
        AccountId::from_hex(&faucet_hex).map_err(|e| format!("Invalid faucet: {}", e))?;

    print_info("Enter the amount:");
    let amount_str = prompt_input(editor, "  Amount: ")?;
    let amount: u64 = amount_str
        .trim()
        .parse()
        .map_err(|e| format!("Invalid amount: {}", e))?;

    println!("\nTransfer details:");
    println!("  Recipient: {}", shorten_hex(&recipient_hex));
    println!("  Faucet:    {}", shorten_hex(&faucet_hex));
    println!("  Amount:    {}", amount);

    let confirm = prompt_input(editor, "\nConfirm? [y/N]: ")?;
    if confirm.to_lowercase() != "y" {
        return Err("Cancelled".to_string());
    }

    Ok(TransactionType::transfer(recipient, faucet_id, amount))
}

async fn prompt_consume_notes(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<TransactionType, String> {
    let client = state.get_client_mut()?;

    print_waiting("Fetching consumable notes...");
    let notes = client
        .list_consumable_notes()
        .await
        .map_err(|e| format!("Failed to list notes: {}", e))?;

    if notes.is_empty() {
        return Err("No consumable notes available".to_string());
    }

    println!("\nConsumable notes:");
    for (idx, note) in notes.iter().enumerate() {
        println!("  [{}] {}", idx + 1, shorten_hex(&note.id.to_hex()));

        for asset in &note.assets {
            match asset {
                Asset::Fungible(f) => {
                    println!(
                        "      - {} tokens (faucet: {})",
                        f.amount(),
                        shorten_hex(&f.faucet_id().to_hex())
                    );
                }
                Asset::NonFungible(nft) => {
                    println!("      - NFT (faucet: {:?})", nft.faucet_id_prefix());
                }
            }
        }
    }

    print_info("\nEnter note numbers to consume (comma-separated, e.g., 1,2,3):");
    let selection = prompt_input(editor, "  Notes: ")?;

    let indices: Vec<usize> = selection
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .collect();

    if indices.is_empty() {
        return Err("No valid notes selected".to_string());
    }

    let note_ids: Vec<NoteId> = indices
        .iter()
        .filter_map(|&i| notes.get(i.saturating_sub(1)).map(|n| n.id))
        .collect();

    if note_ids.is_empty() {
        return Err("No valid notes selected".to_string());
    }

    println!("\nSelected {} note(s)", note_ids.len());
    let confirm = prompt_input(editor, "Confirm? [y/N]: ")?;
    if confirm.to_lowercase() != "y" {
        return Err("Cancelled".to_string());
    }

    Ok(TransactionType::consume_notes(note_ids))
}

fn prompt_switch_psm(
    state: &SessionState,
    editor: &mut DefaultEditor,
) -> Result<TransactionType, String> {
    let client = state.get_client()?;
    print_info(&format!("Current PSM: {}", client.psm_endpoint()));

    print_info("\nEnter new PSM server details:");
    let new_endpoint = prompt_input(editor, "  New PSM endpoint: ")?;
    if new_endpoint.is_empty() {
        return Err("Endpoint is required".to_string());
    }

    let pubkey_hex = prompt_input(editor, "  New PSM pubkey commitment: ")?;
    if pubkey_hex.is_empty() {
        return Err("Pubkey commitment is required".to_string());
    }

    let new_commitment = commitment_from_hex(&ensure_hex_prefix(&pubkey_hex))
        .map_err(|e| format!("Invalid pubkey: {}", e))?;

    println!("\nPSM switch details:");
    println!("  New endpoint: {}", new_endpoint);
    println!("  New pubkey:   {}", shorten_hex(&pubkey_hex));

    print_info("\n⚠️  WARNING: After execution, all future transactions use the new PSM.");

    let confirm = prompt_input(editor, "\nConfirm PSM switch? [y/N]: ")?;
    if confirm.to_lowercase() != "y" {
        return Err("Cancelled".to_string());
    }

    Ok(TransactionType::switch_psm(new_endpoint, new_commitment))
}

/// Print details of an exported proposal.
fn print_proposal_details(proposal: &ExportedProposal) {
    let (collected, required) = proposal.signature_counts();

    println!("\nProposal Details:");
    println!("  ID:           {}", shorten_hex(&proposal.id));
    println!("  Account:      {}", shorten_hex(&proposal.account_id));
    println!("  Type:         {}", proposal.transaction_type);
    println!("  Nonce:        {}", proposal.nonce);
    println!("  Signatures:   {}/{}", collected, required);

    let needed = proposal.signatures_needed();
    if needed == 0 {
        println!("  Status:       ✓ Ready for execution");
    } else {
        println!("  Status:       Pending ({} more needed)", needed);
    }

    let signers = proposal.signed_by();
    if !signers.is_empty() {
        println!("  Signers:");
        for signer in signers {
            println!("    - {}", shorten_hex(signer));
        }
    }
}
