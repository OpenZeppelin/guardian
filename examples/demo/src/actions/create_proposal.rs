use miden_multisig_client::{commitment_from_hex, TransactionType};
use rustyline::DefaultEditor;

use crate::display::{print_error, print_info, print_section, print_success, shorten_hex};
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
    println!("    [b] Back to main menu");
    println!();

    let choice = prompt_input(editor, "Choice: ")?;

    match choice.as_str() {
        "1" => create_add_cosigner_proposal(state, editor, threshold, current_num_cosigners).await,
        "2" => {
            create_remove_cosigner_proposal(state, editor, threshold, current_num_cosigners).await
        }
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
