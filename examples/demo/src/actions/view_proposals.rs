use miden_multisig_client::ProposalStatus;

use crate::display::{print_full_hex, print_info, print_section, shorten_hex};
use crate::state::SessionState;

pub async fn action_view_proposals(state: &mut SessionState) -> Result<(), String> {
    print_section("View Pending Proposals");

    let client = state.get_client_mut()?;

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
        println!("  [{}] Proposal", idx + 1);
        println!("      Type: {:?}", proposal.transaction_type);
        print_full_hex("      Proposal ID", &proposal.id);

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

        println!();
    }

    print_info("\nUse option [8] to sign a proposal");
    print_info("Use option [9] to finalize a proposal");

    Ok(())
}
