use rand::RngCore;
use rustyline::DefaultEditor;

use crate::display::{print_section, print_success, print_waiting, shorten_hex};
use crate::menu::prompt_input;
use crate::multisig::create_multisig_psm_account;
use crate::state::SessionState;

pub async fn action_create_account(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Create Multisig Account");

    let threshold: u64 = prompt_input(editor, "Enter threshold (e.g., 2): ")?
        .parse()
        .map_err(|_| "Invalid threshold")?;

    let num_cosigners: usize = prompt_input(editor, "Enter number of cosigners (including you): ")?
        .parse()
        .map_err(|_| "Invalid number")?;

    if num_cosigners < threshold as usize {
        return Err("Number of cosigners must be >= threshold".to_string());
    }

    let mut cosigner_commitments = Vec::new();

    let user_commitment = state.get_commitment_hex()?;

    cosigner_commitments.push(user_commitment.to_string());

    println!("\nYour commitment: {}", shorten_hex(user_commitment));
    println!("\nEnter commitments for other cosigners:");

    for i in 1..num_cosigners {
        let commitment = prompt_input(editor, &format!("  Cosigner {} commitment: ", i + 1))?;

        let commitment_stripped = commitment.strip_prefix("0x").unwrap_or(&commitment);
        if commitment_stripped.len() != 64 {
            return Err(format!(
                "Invalid commitment length for cosigner {}: expected 64 hex chars, got {}",
                i + 1,
                commitment_stripped.len()
            ));
        }

        hex::decode(commitment_stripped)
            .map_err(|_| format!("Invalid commitment hex for cosigner {}", i + 1))?;

        let commitment_with_prefix = if commitment.starts_with("0x") {
            commitment
        } else {
            format!("0x{}", commitment)
        };

        cosigner_commitments.push(commitment_with_prefix);
    }

    let psm_client = state.get_psm_client_mut()?;
    print_waiting("Fetching PSM server commitment");

    let psm_commitment_hex = psm_client
        .get_pubkey()
        .await
        .map_err(|e| format!("Failed to get PSM commitment: {}", e))?;

    println!("PSM Commitment: {}", shorten_hex(&psm_commitment_hex));

    print_waiting("Creating multisig account");

    let mut rng = state.create_rng();
    let mut init_seed = [0u8; 32];
    rng.fill_bytes(&mut init_seed);

    let cosigner_refs: Vec<&str> = cosigner_commitments.iter().map(|s| s.as_str()).collect();
    let account =
        create_multisig_psm_account(threshold, &cosigner_refs, &psm_commitment_hex, init_seed);

    print_waiting("Adding account to Miden client");

    let account_id = account.id();
    let miden_client = state.get_miden_client_mut()?;
    miden_client
        .add_account(&account, false)
        .await
        .map_err(|e| e.to_string())?;

    miden_client
        .sync_state()
        .await
        .map_err(|e| format!("Failed to sync client state: {}", e))?;

    state.set_account(account);
    state.cosigner_commitments = cosigner_commitments;

    print_success(&format!(
        "Account created: {}",
        shorten_hex(&account_id.to_string())
    ));

    Ok(())
}
