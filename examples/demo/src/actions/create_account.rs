use miden_multisig_client::{commitment_from_hex, ProcedureName, ProcedureThreshold};
use rustyline::DefaultEditor;

use crate::display::{print_section, print_success, print_waiting, shorten_hex};
use crate::menu::prompt_input;
use crate::state::SessionState;

pub async fn action_create_account(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    print_section("Create Multisig Account");

    let threshold: u32 = prompt_input(editor, "Enter threshold (e.g., 2): ")?
        .parse()
        .map_err(|_| "Invalid threshold")?;

    let num_cosigners: usize = prompt_input(editor, "Enter number of cosigners (including you): ")?
        .parse()
        .map_err(|_| "Invalid number")?;

    if num_cosigners < threshold as usize {
        return Err("Number of cosigners must be >= threshold".to_string());
    }

    let mut procedure_thresholds = Vec::new();
    let use_proc_thresholds = prompt_input(editor, "Configure per-procedure thresholds? [y/N]: ")?;
    if use_proc_thresholds.to_lowercase() == "y" {
        println!("\nAvailable procedures:");
        for procedure in ProcedureName::all() {
            println!("  - {}", procedure);
        }
        println!("Leave procedure name empty to finish.\n");

        loop {
            let procedure_name = prompt_input(editor, "  Procedure name: ")?;
            if procedure_name.is_empty() {
                break;
            }

            let procedure: ProcedureName = procedure_name
                .parse()
                .map_err(|_| format!("Unknown procedure: {}", procedure_name))?;

            if procedure_thresholds
                .iter()
                .any(|pt: &ProcedureThreshold| pt.procedure == procedure)
            {
                return Err(format!("Duplicate threshold override for {}", procedure));
            }

            let procedure_threshold: u32 = prompt_input(editor, "  Threshold override: ")?
                .parse()
                .map_err(|_| "Invalid threshold override".to_string())?;

            if procedure_threshold == 0 {
                return Err("Threshold override must be >= 1".to_string());
            }

            if procedure_threshold as usize > num_cosigners {
                return Err(format!(
                    "Threshold override {} exceeds number of signers {}",
                    procedure_threshold, num_cosigners
                ));
            }

            procedure_thresholds.push(ProcedureThreshold::new(procedure, procedure_threshold));
        }
    }

    let mut cosigner_commitment_hexes = Vec::new();

    let user_commitment_hex = state.user_commitment_hex()?;
    cosigner_commitment_hexes.push(user_commitment_hex.clone());

    println!("\nYour commitment: {}", shorten_hex(&user_commitment_hex));
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

        cosigner_commitment_hexes.push(commitment_with_prefix);
    }

    // Convert hex strings to Words
    let signer_commitments: Vec<_> = cosigner_commitment_hexes
        .iter()
        .map(|hex| commitment_from_hex(hex))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse commitment: {}", e))?;

    print_waiting("Creating multisig account");

    let client = state.get_client_mut()?;
    let account = client
        .create_account_with_proc_thresholds(threshold, signer_commitments, procedure_thresholds)
        .await
        .map_err(|e| format!("Failed to create account: {}", e))?;

    let account_id = account.id();

    print_success(&format!(
        "Account created: {}",
        shorten_hex(&account_id.to_string())
    ));

    // Automatically configure account in PSM
    print_waiting("Configuring account in PSM");

    let client = state.get_client_mut()?;
    client
        .push_account()
        .await
        .map_err(|e| format!("PSM configuration failed: {}", e))?;

    print_success("Account configured in PSM");

    Ok(())
}
