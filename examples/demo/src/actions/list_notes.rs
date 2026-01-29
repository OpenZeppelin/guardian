use miden_multisig_client::Asset;

use crate::display::{print_info, print_section, print_success, print_waiting, shorten_hex};
use crate::state::SessionState;

pub async fn action_list_notes(state: &mut SessionState) -> Result<(), String> {
    print_section("Notes");

    let client = state.get_client_mut()?;

    // Fetch notes with their status for detailed information
    print_waiting("Fetching notes with status...");
    let notes_with_status = client
        .list_notes_with_status()
        .await
        .map_err(|e| format!("Failed to list notes: {}", e))?;

    println!();

    if notes_with_status.is_empty() {
        print_info("No notes found");
        println!();
        print_info("Tip: Run 'Sync account' to refresh local state from the network.");
        print_info("     Notes may take a few blocks to be committed on-chain.");
        return Ok(());
    }

    print_success(&format!("Found {} note(s):", notes_with_status.len()));
    println!();

    let mut consumable_count = 0;

    for (idx, (note, statuses)) in notes_with_status.iter().enumerate() {
        let note_id_hex = note.id.to_hex();
        println!("  [{}] Note ID: {}", idx + 1, shorten_hex(&note_id_hex));

        // Show status
        for (account_id, status) in statuses {
            let is_our_account = client
                .account_id()
                .map(|id| id == *account_id)
                .unwrap_or(false);
            let marker = if is_our_account { " (our account)" } else { "" };
            println!(
                "      Status: {} for {}{}",
                status,
                shorten_hex(&account_id.to_hex()),
                marker
            );

            // Check for consumable statuses (both Consumable and ConsumableWithAuthorization)
            if is_our_account
                && (status == "Consumable" || status == "ConsumableWithAuthorization")
            {
                consumable_count += 1;
            }
        }

        if note.assets.is_empty() {
            println!("      Assets: (none)");
        } else {
            println!("      Assets:");
            for asset in &note.assets {
                match asset {
                    Asset::Fungible(fungible) => {
                        println!(
                            "        - {} tokens (faucet: {})",
                            fungible.amount(),
                            shorten_hex(&fungible.faucet_id().to_hex())
                        );
                    }
                    Asset::NonFungible(nft) => {
                        println!(
                            "        - NFT (faucet prefix: {})",
                            shorten_hex(&format!("{:?}", nft.faucet_id_prefix()))
                        );
                    }
                }
            }
        }
        println!();
    }

    if consumable_count > 0 {
        print_success(&format!("{} note(s) ready to consume", consumable_count));
        print_info("Use 'Create Proposal' > 'Consume notes' to consume these notes");
    } else {
        print_info("No notes are consumable yet.");
        print_info("Tip: Wait for the note to be fully processed (may take a few blocks).");
        print_info("     Then run 'Sync account' again to update the status.");
    }

    Ok(())
}
