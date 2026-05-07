mod actions;
mod display;
mod menu;
mod state;

use miden_client::rpc::Endpoint;
use miden_multisig_client::SignatureScheme;
use rustyline::DefaultEditor;

use actions::{
    action_create_account, action_list_notes, action_proposal_management, action_recover_by_key,
    action_show_account, action_show_status, action_sync_account, action_verify_state_commitment,
};
use display::{
    print_banner, print_error, print_full_hex, print_section, print_success, print_waiting,
    shorten_hex_32,
};
use menu::{handle_invalid_choice, parse_menu_choice, prompt_input, MenuAction};
use state::SessionState;

async fn startup(editor: &mut DefaultEditor) -> Result<SessionState, String> {
    print_banner();

    print_section("Configuration");

    // Network selection menu
    println!("\n  Select Miden network:");
    println!("    [1] Local (http://localhost:57291)");
    println!("    [2] Devnet (https://rpc.devnet.miden.io)");
    println!("    [3] Testnet (https://rpc.testnet.miden.io)");
    println!("    [4] Custom URL");
    println!();

    let network_choice = prompt_input(editor, "Network [1]: ")?;
    let miden_endpoint = match network_choice.trim() {
        "" | "1" => Endpoint::new("http".to_string(), "localhost".to_string(), Some(57291)),
        "2" => Endpoint::new("https".to_string(), "rpc.devnet.miden.io".to_string(), None),
        "3" => Endpoint::new(
            "https".to_string(),
            "rpc.testnet.miden.io".to_string(),
            None,
        ),
        "4" => {
            let custom_url = prompt_input(editor, "Enter Miden Node URL: ")?;
            parse_miden_endpoint(&custom_url)?
        }
        _ => {
            println!("  Invalid choice, using local");
            Endpoint::new("http".to_string(), "localhost".to_string(), Some(57291))
        }
    };

    // GUARDIAN endpoint selection
    println!("\n  Select GUARDIAN gRPC server:");
    println!("    [1] Local gRPC (http://localhost:50051)");
    println!("    [2] Custom gRPC URL");
    println!();

    let guardian_choice = prompt_input(editor, "GUARDIAN Server [1]: ")?;
    let guardian_endpoint = match guardian_choice.trim() {
        "" | "1" => "http://localhost:50051".to_string(),
        "2" => prompt_input(editor, "Enter GUARDIAN gRPC URL: ")?,
        _ => {
            println!("  Invalid choice, using local gRPC");
            "http://localhost:50051".to_string()
        }
    };

    println!("\n  GUARDIAN Server: {}", guardian_endpoint);
    println!(
        "  Miden Node: {}://{}{}",
        miden_endpoint.protocol(),
        miden_endpoint.host(),
        miden_endpoint
            .port()
            .map(|p| format!(":{}", p))
            .unwrap_or_default()
    );

    println!("\n  Select signature scheme:");
    println!("    [1] Falcon");
    println!("    [2] ECDSA");
    println!();

    let scheme_choice = prompt_input(editor, "Signature scheme [1]: ")?;
    let signature_scheme = match scheme_choice.trim() {
        "" | "1" => SignatureScheme::Falcon,
        "2" => SignatureScheme::Ecdsa,
        _ => {
            println!("  Invalid choice, using Falcon");
            SignatureScheme::Falcon
        }
    };

    let scheme_name = match signature_scheme {
        SignatureScheme::Falcon => "Falcon",
        SignatureScheme::Ecdsa => "ECDSA",
    };

    print_waiting(&format!(
        "Initializing MultisigClient with new {} keypair",
        scheme_name
    ));

    let mut state = SessionState::new()?;
    state
        .initialize_client(miden_endpoint, &guardian_endpoint, signature_scheme)
        .await?;

    let commitment_hex = state.user_commitment_hex()?;

    print_success("Client initialized!");
    println!("  Signature scheme: {}", state.signature_scheme_name());
    if state.is_ecdsa() {
        println!("  Your commitment: {}", shorten_hex_32(&commitment_hex));
        print_full_hex("  Your commitment (full)", &commitment_hex);
    } else {
        print_full_hex("  Your commitment", &commitment_hex);
    }
    println!("\n  Share this commitment with other cosigners to be added to multisig accounts.");

    Ok(state)
}

fn parse_miden_endpoint(input: &str) -> Result<Endpoint, String> {
    if !input.starts_with("http://") && !input.starts_with("https://") {
        return Err("Miden endpoint must start with http:// or https://".to_string());
    }

    let url_parts: Vec<&str> = input.split("://").collect();
    if url_parts.len() != 2 {
        return Err("Invalid Miden endpoint format".to_string());
    }

    let protocol = url_parts[0];
    let rest = url_parts[1];

    let (host, port) = if rest.contains(':') {
        let parts: Vec<&str> = rest.split(':').collect();
        let port = parts[1].parse::<u16>().map_err(|_| "Invalid port number")?;
        (parts[0].to_string(), Some(port))
    } else {
        (rest.to_string(), None)
    };

    Ok(Endpoint::new(protocol.to_string(), host, port))
}

async fn handle_action(
    action: MenuAction,
    state: &mut SessionState,
    editor: &mut DefaultEditor,
) -> Result<(), String> {
    match action {
        MenuAction::CreateAccount => action_create_account(state, editor).await,
        MenuAction::SyncAccount => action_sync_account(state, editor).await,
        MenuAction::VerifyStateCommitment => action_verify_state_commitment(state).await,
        MenuAction::ListNotes => action_list_notes(state).await,
        MenuAction::ProposalManagement => action_proposal_management(state, editor).await,
        MenuAction::RecoverByKey => action_recover_by_key(state).await,
        MenuAction::ShowAccount => action_show_account(state).await,
        MenuAction::ShowStatus => action_show_status(state).await,
        MenuAction::Quit => {
            println!("\nGoodbye!");
            std::process::exit(0);
        }
    }
}

#[tokio::main]
async fn main() {
    let mut editor = DefaultEditor::new().expect("Failed to create editor");

    let mut state = match startup(&mut editor).await {
        Ok(s) => s,
        Err(e) => {
            print_error(&format!("Startup failed: {}", e));
            std::process::exit(1);
        }
    };

    loop {
        menu::print_menu(&state);

        let choice = match menu::get_user_choice(&mut editor) {
            Ok(c) => c,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("\nInterrupted");
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("\nGoodbye!");
                break;
            }
            Err(e) => {
                print_error(&format!("Input error: {}", e));
                continue;
            }
        };

        match parse_menu_choice(&choice, &state) {
            Some(action) => {
                if let Err(e) = handle_action(action, &mut state, &mut editor).await {
                    print_error(&e);
                }
            }
            None => handle_invalid_choice(),
        }
    }
}
