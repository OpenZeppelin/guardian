use crate::display::{print_full_hex, print_info, print_section, shorten_hex};
use crate::proposals::{count_signatures, extract_proposal_metadata, get_signers};
use crate::state::SessionState;

pub async fn action_view_proposals(state: &mut SessionState) -> Result<(), String> {
    print_section("View Pending Proposals");

    let account = state.get_account()?;
    let account_id = account.id();

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

    for (idx, proposal) in proposals.iter().enumerate() {
        let metadata = extract_proposal_metadata(proposal);
        let signature_count = count_signatures(proposal);
        let signers = get_signers(proposal);
        let tx_commitment = metadata.get_tx_commitment();

        println!("  [{}] Proposal (nonce: {})", idx + 1, proposal.nonce);
        println!("      Type: {}", metadata.proposal_type);

        if let Some(ref commitment) = tx_commitment {
            print_full_hex("      Proposal ID", commitment);
        }

        println!("      Signatures: {}", signature_count);

        if !signers.is_empty() {
            println!("      Signers:");
            for signer in &signers {
                println!("        - {}", shorten_hex(signer));
            }
        }

        println!();
    }

    print_info("\nUse option [8] to sign a proposal");
    print_info("Use option [9] to finalize a proposal");

    Ok(())
}
