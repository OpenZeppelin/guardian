use miden_multisig_client::ProposalStatus;

use crate::display::{print_info, print_section, print_success, print_waiting, shorten_hex};
use crate::state::SessionState;

pub async fn action_execute_proposal(state: &mut SessionState) -> Result<(), String> {
    print_section("Execute Proposal");

    let client = state.get_client_mut()?;

    print_waiting("Fetching pending proposals from PSM");
    let proposals = client
        .list_proposals()
        .await
        .map_err(|e| format!("Failed to get proposals: {}", e))?;

    if proposals.is_empty() {
        print_info("No pending proposals found");
        return Err("No proposals to execute".to_string());
    }

    println!("\nPending Proposals:");
    for (idx, proposal) in proposals.iter().enumerate() {
        println!("  [{}] Proposal: {}", idx + 1, shorten_hex(&proposal.id));
        println!("      Type: {:?}", proposal.transaction_type);

        if let ProposalStatus::Pending {
            signatures_collected,
            signatures_required,
            signers,
        } = &proposal.status
        {
            println!(
                "      Signatures: {}/{}",
                signatures_collected, signatures_required
            );
            if !signers.is_empty() {
                println!("      Signers:");
                for signer in signers {
                    println!("        - {}", shorten_hex(signer));
                }
            }
        }
    }

    print!("\nSelect proposal number to execute: ");
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

    let proposal_id = proposals[idx - 1].id.clone();

    print_waiting("Executing proposal");

    let client = state.get_client_mut()?;
    client
        .execute_proposal(&proposal_id)
        .await
        .map_err(|e| format!("Failed to execute proposal: {}", e))?;

    print_success("Transaction executed successfully!");

    print_waiting("Syncing account from PSM");
    client
        .sync_account()
        .await
        .map_err(|e| format!("Failed to sync account: {}", e))?;

    let account = client
        .account()
        .ok_or_else(|| "No account loaded".to_string())?;
    print_success(&format!(
        "Account synced. New nonce: {}",
        account.nonce()
    ));

    Ok(())
}
