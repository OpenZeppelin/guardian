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
    let commitment_display = if state.is_ecdsa() {
        shorten_hex_32(&commitment)
    } else {
        shorten_hex(&commitment)
    };
    println!("  Your Commitment: {}", commitment_display);
    if state.is_ecdsa() {
        println!("  Your Commitment (full): {}", commitment);
    }

    let client = state.get_client()?;
    if let Some(pubkey_hex) = client.key_manager().public_key_hex() {
        let pubkey_display = if state.is_ecdsa() {
            shorten_hex_32(&pubkey_hex)
        } else {
            shorten_hex(&pubkey_hex)
        };
        println!("  Your Public Key: {}", pubkey_display);
        if state.is_ecdsa() {
            println!("  Your Public Key (full): {}", pubkey_hex);
        }
    }

    Ok(())
}
