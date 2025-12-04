use miden_multisig_client::{commitment_from_hex, Asset, NoteId, TransactionType};
use miden_objects::account::AccountId;
use rustyline::DefaultEditor;

use crate::display::{
    print_error, print_info, print_section, print_success, print_waiting, shorten_hex,
};
use crate::menu::prompt_input;
use crate::state::SessionState;

pub async fn action_create_proposal(
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
    println!("    [1] Add cosigner (update to N+1)");
    println!("    [2] Remove cosigner (update to N-1)");
    println!("    [3] Transfer assets (P2ID)");
    println!("    [4] Consume notes");
    println!("    [b] Back to main menu");
    println!();

    let choice = prompt_input(editor, "Choice: ")?;

    match choice.as_str() {
        "1" => create_add_cosigner_proposal(state, editor, threshold, current_num_cosigners).await,
        "2" => {
            create_remove_cosigner_proposal(state, editor, threshold, current_num_cosigners).await
        }
        "3" => create_p2id_proposal(state, editor, threshold).await,
        "4" => create_consume_notes_proposal(state, editor, threshold).await,
        "b" | "B" => {
            print_info("Returning to main menu");
            Ok(())
        }
        _ => {
            print_error("Invalid choice");
            Ok(())
        }
    }
}

async fn create_add_cosigner_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
    threshold: u32,
    current_num_cosigners: usize,
) -> Result<(), String> {
    print_section("Add Cosigner");

    print_info(&format!(
        "New config will be: {}-of-{}",
        threshold,
        current_num_cosigners + 1
    ));

    print_info("Enter the new cosigner's commitment:");
    let new_cosigner_commitment_hex = prompt_input(editor, "  New cosigner commitment: ")?;

    let new_commitment = parse_commitment(&new_cosigner_commitment_hex)?;

    let client = state.get_client_mut()?;

    let proposal = client
        .propose_transaction(TransactionType::AddCosigner { new_commitment })
        .await
        .map_err(|e| format!("Failed to create add_cosigner proposal: {}", e))?;

    print_proposal_success(client.user_commitment_hex(), &proposal.id, threshold);

    Ok(())
}

async fn create_remove_cosigner_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
    threshold: u32,
    current_num_cosigners: usize,
) -> Result<(), String> {
    print_section("Remove Cosigner");

    if current_num_cosigners <= 1 {
        return Err("Cannot remove cosigner: only one cosigner remaining".to_string());
    }

    let new_threshold = std::cmp::min(threshold, (current_num_cosigners - 1) as u32);
    print_info(&format!(
        "New config will be: {}-of-{}",
        new_threshold,
        current_num_cosigners - 1
    ));

    // Show current cosigners
    let client = state.get_client()?;
    let account = client
        .account()
        .ok_or_else(|| "No account loaded".to_string())?;
    let cosigners = account.cosigner_commitments();

    println!();
    print_info("Current cosigners:");
    for (i, commitment) in cosigners.iter().enumerate() {
        let hex = format!(
            "0x{}",
            hex::encode(
                commitment
                    .iter()
                    .flat_map(|f| f.as_int().to_le_bytes())
                    .collect::<Vec<_>>()
            )
        );
        println!("    [{}] {}", i + 1, shorten_hex(&hex));
    }
    println!();

    print_info("Enter the commitment of the cosigner to remove:");
    let commitment_hex = prompt_input(editor, "  Cosigner commitment to remove: ")?;

    let commitment = parse_commitment(&commitment_hex)?;

    let client = state.get_client_mut()?;

    let proposal = client
        .propose_transaction(TransactionType::RemoveCosigner { commitment })
        .await
        .map_err(|e| format!("Failed to create remove_cosigner proposal: {}", e))?;

    print_proposal_success(client.user_commitment_hex(), &proposal.id, threshold);

    Ok(())
}

async fn create_p2id_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
    threshold: u32,
) -> Result<(), String> {
    print_section("Transfer Assets (P2ID)");

    // Prompt for recipient account ID
    print_info("Enter the recipient account ID:");
    let recipient_hex = prompt_input(editor, "  Recipient account ID: ")?;

    let recipient = AccountId::from_hex(&recipient_hex)
        .map_err(|e| format!("Invalid recipient account ID: {}", e))?;

    // Prompt for faucet ID
    print_info("Enter the faucet/asset ID:");
    let faucet_hex = prompt_input(editor, "  Faucet ID: ")?;

    let faucet_id =
        AccountId::from_hex(&faucet_hex).map_err(|e| format!("Invalid faucet ID: {}", e))?;

    // Prompt for amount
    print_info("Enter the amount to transfer:");
    let amount_str = prompt_input(editor, "  Amount: ")?;

    let amount: u64 = amount_str
        .trim()
        .parse()
        .map_err(|e| format!("Invalid amount: {}", e))?;

    // Confirm the transfer details
    println!();
    print_info("Transfer details:");
    print_info(&format!("  Recipient: {}", shorten_hex(&recipient_hex)));
    print_info(&format!("  Faucet:    {}", shorten_hex(&faucet_hex)));
    print_info(&format!("  Amount:    {}", amount));
    println!();

    let confirm = prompt_input(editor, "Confirm transfer? (y/n): ")?;
    if confirm.to_lowercase() != "y" {
        print_info("Transfer cancelled");
        return Ok(());
    }

    let client = state.get_client_mut()?;

    let proposal = client
        .propose_transaction(TransactionType::P2ID {
            recipient,
            faucet_id,
            amount,
        })
        .await
        .map_err(|e| format!("Failed to create P2ID proposal: {}", e))?;

    print_proposal_success(client.user_commitment_hex(), &proposal.id, threshold);

    Ok(())
}

async fn create_consume_notes_proposal(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
    threshold: u32,
) -> Result<(), String> {
    print_section("Consume Notes");

    let client = state.get_client_mut()?;

    // List available notes
    print_waiting("Fetching consumable notes...");
    let notes = client
        .list_consumable_notes()
        .await
        .map_err(|e| format!("Failed to list notes: {}", e))?;

    if notes.is_empty() {
        print_info("No consumable notes available");
        print_info("(Notes must be committed on-chain to be consumable)");
        return Ok(());
    }

    println!();
    print_info(&format!("Found {} consumable note(s):", notes.len()));
    println!();

    // Display notes for selection
    for (idx, note) in notes.iter().enumerate() {
        let note_id_hex = note.id.to_hex();
        println!("  [{}] Note ID: {}", idx + 1, shorten_hex(&note_id_hex));

        if !note.assets.is_empty() {
            for asset in &note.assets {
                match asset {
                    Asset::Fungible(fungible) => {
                        println!(
                            "      - {} tokens (faucet: {})",
                            fungible.amount(),
                            shorten_hex(&fungible.faucet_id().to_hex())
                        );
                    }
                    Asset::NonFungible(nft) => {
                        println!(
                            "      - NFT (faucet prefix: {})",
                            shorten_hex(&format!("{:?}", nft.faucet_id_prefix()))
                        );
                    }
                }
            }
        }
    }
    println!();

    // Prompt for selection (comma-separated indices)
    print_info("Enter the note numbers to consume (comma-separated, e.g., 1,2,3):");
    let selection = prompt_input(editor, "  Notes to consume: ")?;

    // Parse selection
    let indices: Vec<usize> = selection
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .collect();

    if indices.is_empty() {
        print_error("No valid note numbers entered");
        return Ok(());
    }

    let note_ids: Vec<NoteId> = indices
        .iter()
        .filter_map(|&i| notes.get(i.saturating_sub(1)).map(|n| n.id))
        .collect();

    if note_ids.is_empty() {
        print_error("No valid notes selected (check note numbers)");
        return Ok(());
    }

    // Confirm
    println!();
    print_info(&format!("Selected {} note(s) to consume", note_ids.len()));
    let confirm = prompt_input(editor, "Confirm? (y/n): ")?;
    if confirm.to_lowercase() != "y" {
        print_info("Cancelled");
        return Ok(());
    }

    // Create proposal
    let proposal = client
        .propose_transaction(TransactionType::ConsumeNotes { note_ids })
        .await
        .map_err(|e| format!("Failed to create consume notes proposal: {}", e))?;

    print_proposal_success(client.user_commitment_hex(), &proposal.id, threshold);

    Ok(())
}

fn parse_commitment(hex_input: &str) -> Result<miden_multisig_client::Word, String> {
    let commitment_stripped = hex_input.strip_prefix("0x").unwrap_or(hex_input);
    if commitment_stripped.len() != 64 {
        return Err(format!(
            "Invalid commitment length: expected 64 hex chars, got {}",
            commitment_stripped.len()
        ));
    }

    let commitment_hex = if hex_input.starts_with("0x") {
        hex_input.to_string()
    } else {
        format!("0x{}", hex_input)
    };

    commitment_from_hex(&commitment_hex).map_err(|e| format!("Invalid commitment: {}", e))
}

fn print_proposal_success(user_commitment_hex: String, proposal_id: &str, threshold: u32) {
    print_success("Proposal created on PSM server");
    print_info(&format!("Proposal ID: {}", proposal_id));

    print_success(&format!(
        "Automatically signed with your key ({})",
        shorten_hex(&user_commitment_hex)
    ));

    print_info("\nNext steps:");
    print_info("  1. Share the Proposal ID above with other cosigners");
    print_info("  2. Other cosigners use option [5] 'Sign a proposal'");
    print_info(&format!(
        "  3. Once you have {} signatures, use option [6] 'Execute a proposal'",
        threshold
    ));
}
