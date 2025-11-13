use crate::display::{print_connection_status, shorten_hex};
use crate::state::SessionState;

pub async fn action_show_status(state: &SessionState) -> Result<(), String> {
    print_connection_status(state.is_psm_connected(), state.is_miden_connected());

    if state.has_account() {
        let account_id = state.get_account_id()?;
        println!(
            "\n  Current Account: {}",
            shorten_hex(&account_id.to_string())
        );
    } else {
        println!("\n  No account loaded");
    }

    if state.has_keypair() {
        let commitment = state.get_commitment_hex()?;
        println!("  Your Commitment: {}", shorten_hex(commitment));
    } else {
        println!("  No keypair generated");
    }

    Ok(())
}
