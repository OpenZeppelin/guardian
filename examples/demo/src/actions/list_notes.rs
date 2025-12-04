use miden_multisig_client::Asset;

use crate::display::{print_info, print_section, print_waiting, shorten_hex};
use crate::state::SessionState;

pub async fn action_list_notes(state: &mut SessionState) -> Result<(), String> {
    print_section("Consumable Notes");

    let client = state.get_client_mut()?;

    print_waiting("Fetching consumable notes...");
    let notes = client
        .list_consumable_notes()
        .await
        .map_err(|e| format!("Failed to list notes: {}", e))?;

    if notes.is_empty() {
        print_info("No consumable notes found");
        print_info("(Notes must be committed on-chain to be consumable)");
        return Ok(());
    }

    println!();
    print_info(&format!("Found {} consumable note(s):", notes.len()));
    println!();

    for (idx, note) in notes.iter().enumerate() {
        let note_id_hex = note.id.to_hex();
        println!("  [{}] Note ID: {}", idx + 1, shorten_hex(&note_id_hex));

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

    print_info("Use 'Create Proposal' > 'Consume notes' to consume these notes");

    Ok(())
}
