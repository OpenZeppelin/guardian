//! Guardian session submenu: the signer signs one grant, then a delegated
//! key signs reads and proposal requests without further prompts.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use miden_multisig_client::{SessionInfo, StartSessionOptions};
use miden_protocol::address::NetworkId;
use rustyline::DefaultEditor;

use crate::display::{print_error, print_full_hex, print_info, print_section, print_success};
use crate::menu::prompt_input;
use crate::state::SessionState;

/// Guardian session submenu: start, show, end and revoke all.
pub async fn action_guardian_session(
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    loop {
        print_session_menu();
        let choice = prompt_input(editor, "Choice: ")?;
        let result = match choice.as_str() {
            "1" => start_session(state, editor).await,
            "2" => show_session(state),
            "3" => end_session(state).await,
            "4" => revoke_all_sessions(state).await,
            "b" | "back" => return Ok(()),
            _ => Err("Invalid choice".to_string()),
        };
        if let Err(error) = result {
            print_error(&error);
        }
    }
}

fn print_session_menu() {
    println!("\n┌─────────────────────────────────────────────┐");
    println!("│ Guardian Session                            │");
    println!("└─────────────────────────────────────────────┘");
    println!("  [1] Start session (signer signs one grant)");
    println!("  [2] Show session");
    println!("  [3] End session");
    println!("  [4] Revoke all sessions of this signer");
    println!();
    println!("  [b] Back to main menu");
    println!();
}

async fn start_session(state: &mut SessionState, editor: &mut DefaultEditor) -> Result<(), String> {
    print_section("Start Guardian Session");
    let default_network = match state.network_id() {
        NetworkId::Testnet => "testnet",
        _ => "devnet",
    };
    let network = prompt_input(
        editor,
        &format!("Guardian network (local/devnet/testnet) [{default_network}]: "),
    )?;
    let network = if network.is_empty() {
        default_network.to_string()
    } else {
        network
    };
    let hours = prompt_input(editor, "Lifetime in hours (1-8) [1]: ")?;
    let hours: u64 = if hours.is_empty() {
        1
    } else {
        hours
            .parse()
            .map_err(|_| "Lifetime must be a whole number of hours".to_string())?
    };

    let info = state
        .get_client_mut()?
        .start_session(StartSessionOptions {
            network,
            ttl: Duration::from_secs(hours * 3600),
        })
        .await
        .map_err(|e| format!("Failed to start session: {e}"))?;
    print_success("Session started: reads and proposal requests no longer use the signer");
    print_session(&info);
    Ok(())
}

fn show_session(state: &SessionState) -> Result<(), String> {
    print_section("Guardian Session");
    match state.get_client()?.session() {
        Some(info) => print_session(&info),
        None => print_info("No active session: every request is signed by the signer"),
    }
    Ok(())
}

async fn end_session(state: &SessionState) -> Result<(), String> {
    let revoked = state
        .get_client()?
        .end_session()
        .await
        .map_err(|e| format!("Failed to end session: {e}"))?;
    if revoked {
        print_success("Session ended");
    } else {
        print_info("No session was active");
    }
    Ok(())
}

async fn revoke_all_sessions(state: &SessionState) -> Result<(), String> {
    let revoked = state
        .get_client()?
        .revoke_all_sessions()
        .await
        .map_err(|e| format!("Failed to revoke sessions: {e}"))?;
    print_success(&format!("Revoked {revoked} session(s) of this signer"));
    print_info("Run it again in 10 minutes if a session key may be compromised");
    Ok(())
}

fn print_session(info: &SessionInfo) {
    print_full_hex("  Session key", &info.session_public_key);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    let minutes_left = info.expires_at.saturating_sub(now) / 60;
    println!(
        "  Expires: unix {} (in {minutes_left} min)",
        info.expires_at
    );
}
