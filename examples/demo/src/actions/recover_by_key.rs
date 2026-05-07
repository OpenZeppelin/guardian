use crate::display::{print_full_hex, print_section, print_success, print_waiting};
use crate::state::SessionState;

pub async fn action_recover_by_key(state: &SessionState) -> Result<(), String> {
    print_section("Recover by Key");
    print_waiting("Looking up accounts authorized by your signer commitment");

    let client = state.get_client()?;
    let recovered = client
        .recover_by_key()
        .await
        .map_err(|e| format!("Recovery lookup failed: {}", e))?;

    if recovered.is_empty() {
        println!("  No accounts on this GUARDIAN authorize this signer commitment.");
        println!("  (The user may have switched operators in the past — try another.)");
        return Ok(());
    }

    print_success(&format!(
        "Found {} account{}:",
        recovered.len(),
        if recovered.len() == 1 { "" } else { "s" }
    ));
    for entry in &recovered {
        print_full_hex("  Account", &entry.account_id);
        let commitment = entry
            .state
            .state
            .as_ref()
            .map(|s| s.commitment.clone())
            .unwrap_or_else(|| "<no state>".to_string());
        print_full_hex("  State commitment", &commitment);
    }

    Ok(())
}
