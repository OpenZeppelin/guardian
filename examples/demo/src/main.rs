mod actions;
mod display;
mod menu;
mod state;

use miden_client::rpc::Endpoint;
use rustyline::DefaultEditor;

use actions::{
    action_create_account, action_list_notes, action_proposal_management, action_show_account,
    action_show_status, action_sync_account,
};
use display::{
    print_banner, print_error, print_full_hex, print_section, print_success, print_waiting,
};
use menu::{handle_invalid_choice, parse_menu_choice, prompt_input, MenuAction};
use state::SessionState;

async fn startup(editor: &mut DefaultEditor) -> Result<SessionState, String> {
    print_banner();

    print_section("Configuration");

    let psm_endpoint = prompt_input(editor, "PSM Server endpoint [http://localhost:50051]: ")?;
    let psm_endpoint = if psm_endpoint.is_empty() {
        "http://localhost:50051".to_string()
    } else {
        psm_endpoint
    };

    let miden_input = prompt_input(
        editor,
        "Miden Node endpoint [https://rpc.testnet.miden.io:443]: ",
    )?;
    let miden_endpoint = if miden_input.is_empty() {
        Endpoint::new(
            "https".to_string(),
            "rpc.testnet.miden.io".to_string(),
            None,
        )
    } else {
        parse_miden_endpoint(&miden_input)?
    };

    println!("\n  PSM Server: {}", psm_endpoint);
    println!(
        "  Miden Node: {}://{}{}",
        miden_endpoint.protocol(),
        miden_endpoint.host(),
        miden_endpoint
            .port()
            .map(|p| format!(":{}", p))
            .unwrap_or_default()
    );

    print_waiting("Initializing MultisigClient with new keypair");

    let mut state = SessionState::new()?;
    state
        .initialize_client(miden_endpoint, &psm_endpoint)
        .await?;

    let commitment_hex = state.user_commitment_hex()?;

    print_success("Client initialized!");
    print_full_hex("  Your commitment", &commitment_hex);
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
        MenuAction::ListNotes => action_list_notes(state).await,
        MenuAction::ProposalManagement => action_proposal_management(state, editor).await,
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
