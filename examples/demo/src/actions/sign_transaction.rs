use miden_multisig_client::ProposalStatus;
use rustyline::DefaultEditor;

use crate::display::{print_info, print_section, print_success, print_waiting, shorten_hex};
use crate::menu::prompt_input;
use crate::state::SessionState;

pub async fn action_sign_transaction(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Sign a Proposal");

    let client = state.get_client_mut()?;

    print_waiting("Fetching proposals from PSM server");
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
        println!("  [{}] Proposal: {}", idx + 1, shorten_hex(&proposal.id));
        println!("      Type: {:?}", proposal.transaction_type);

        if let ProposalStatus::Pending {
            signatures_collected,
            signatures_required,
            ..
        } = &proposal.status
        {
            println!(
                "      Signatures: {}/{}",
                signatures_collected, signatures_required
            );
        }
        println!();
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

    let proposal_id = proposals[idx].id.clone();

    print_waiting("Signing proposal with your key");

    let client = state.get_client_mut()?;
    let updated_proposal = client
        .sign_proposal(&proposal_id)
        .await
        .map_err(|e| format!("Failed to sign proposal: {}", e))?;

    let user_commitment_hex = client.user_commitment_hex();
    print_success(&format!(
        "Signed proposal with key {}",
        shorten_hex(&user_commitment_hex)
    ));

    if let ProposalStatus::Pending {
        signatures_collected,
        signatures_required,
        ..
    } = &updated_proposal.status
    {
        print_info(&format!(
            "Signatures collected: {}/{}",
            signatures_collected, signatures_required
        ));

        if signatures_collected >= signatures_required {
            print_success("\nAll signatures collected!");
            print_info("This proposal can now be finalized using option [9]");
        } else {
            print_info("\nWaiting for more signatures...");
        }
    }

    Ok(())
}
