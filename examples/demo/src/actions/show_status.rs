use crate::display::{shorten_hex, shorten_hex_32};
use crate::state::SessionState;

pub async fn action_show_status(state: &SessionState) -> Result<(), String> {
    println!("\n  Status: Connected");
    println!("  Signature Scheme: {}", state.signature_scheme_name());

    if state.has_account() {
        let client = state.get_client()?;
        let account = client.account().unwrap();
        println!(
            "  Current Account: {}",
            shorten_hex(&account.id().to_string())
        );
    } else {
        println!("  No account loaded");
    }

    let commitment = state.user_commitment_hex()?;
    let display_commitment = if state.is_ecdsa() {
        shorten_hex_32(&commitment)
    } else {
        shorten_hex(&commitment)
    };
    println!("  Your Commitment: {}", display_commitment);

    Ok(())
}
