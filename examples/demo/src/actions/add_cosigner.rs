use miden_multisig_client::{commitment_from_hex, TransactionType};
use rustyline::DefaultEditor;

use crate::display::{print_info, print_section, print_success, shorten_hex};
use crate::menu::prompt_input;
use crate::state::SessionState;

pub async fn action_add_cosigner(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Add Cosigner (Update to N+1)");

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
    print_info(&format!(
        "New config will be: {}-of-{}",
        threshold,
        current_num_cosigners + 1
    ));

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

    let new_commitment = commitment_from_hex(&new_cosigner_commitment_hex)
        .map_err(|e| format!("Invalid commitment: {}", e))?;

    let client = state.get_client_mut()?;

    let proposal = client
        .propose_transaction(TransactionType::AddCosigner { new_commitment })
        .await
        .map_err(|e| format!("Failed to create add_cosigner proposal: {}", e))?;

    print_success("Proposal created on PSM server");
    print_info(&format!("Proposal ID: {}", proposal.id));

    let user_commitment_hex = client.user_commitment_hex();
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

    Ok(())
}
