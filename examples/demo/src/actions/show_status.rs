use crate::display::shorten_hex;
use crate::state::SessionState;

pub async fn action_show_status(state: &SessionState) -> Result<(), String> {
    println!("\n  Status: Connected");

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
    println!("  Your Commitment: {}", shorten_hex(&commitment));

    Ok(())
}
