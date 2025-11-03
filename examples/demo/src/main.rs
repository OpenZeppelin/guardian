mod account_inspector;
mod actions;
mod display;
mod falcon;
mod helpers;
mod menu;
mod multisig;
mod pending_tx;
mod state;

use miden_client::rpc::Endpoint;
use rustyline::DefaultEditor;

use actions::{
    action_add_cosigner, action_configure_psm, action_create_account,
    action_finalize_pending_transaction, action_generate_keypair, action_pull_deltas_from_psm,
    action_pull_from_psm, action_show_account, action_show_status, action_sign_transaction,
};
use display::{print_banner, print_error, print_section, print_success, print_waiting};
use menu::{handle_invalid_choice, parse_menu_choice, MenuAction};
use state::SessionState;

async fn startup() -> Result<SessionState, String> {
    print_banner();

    let mut editor =
        DefaultEditor::new().map_err(|e| format!("Failed to create input editor: {}", e))?;

    print_section("Configuration");

    let psm_endpoint = editor
        .readline("PSM Server endpoint [http://localhost:50051]: ")
        .map_err(|e| format!("Input error: {}", e))?
        .trim()
        .to_string();

    let psm_endpoint = if psm_endpoint.is_empty() {
        "http://localhost:50051".to_string()
    } else {
        psm_endpoint
    };

    let miden_input = editor
        .readline("Miden Node endpoint [http://localhost:57291]: ")
        .map_err(|e| format!("Input error: {}", e))?
        .trim()
        .to_string();

    let miden_endpoint = if miden_input.is_empty() {
        Endpoint::new("http".to_string(), "localhost".to_string(), Some(57291))
    } else {
        if !miden_input.starts_with("http://") && !miden_input.starts_with("https://") {
            return Err("Miden endpoint must start with http:// or https://".to_string());
        }

        let url_parts: Vec<&str> = miden_input.split("://").collect();
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

        Endpoint::new(protocol.to_string(), host, port)
    };

    println!("\n  PSM Server: {}", psm_endpoint);
    println!(
        "  Miden Node: {}://{}{}",
        if matches!(miden_endpoint.port(), Some(443)) {
            "https"
        } else {
            "http"
        },
        miden_endpoint.host(),
        miden_endpoint
            .port()
            .map(|p| format!(":{}", p))
            .unwrap_or_default()
    );

    let mut state = SessionState::new(psm_endpoint, miden_endpoint)?;

    print_waiting("Connecting to PSM server");
    state.connect_psm().await?;
    print_success("Connected to PSM server");

    print_waiting("Connecting to Miden node");
    state.connect_miden().await?;
    print_success("Connected to Miden node");

    Ok(state)
}

async fn handle_action(action: MenuAction, state: &mut SessionState) -> Result<(), String> {
    match action {
        MenuAction::GenerateKeypair => action_generate_keypair(state).await,
        MenuAction::CreateAccount => {
            let mut editor =
                DefaultEditor::new().map_err(|e| format!("Failed to create editor: {}", e))?;
            action_create_account(state, &mut editor).await
        }
        MenuAction::ConfigurePsm => action_configure_psm(state).await,
        MenuAction::PullFromPsm => {
            let mut editor =
                DefaultEditor::new().map_err(|e| format!("Failed to create editor: {}", e))?;
            action_pull_from_psm(state, &mut editor).await
        }
        MenuAction::PullDeltasFromPsm => action_pull_deltas_from_psm(state).await,
        MenuAction::AddCosigner => {
            let mut editor =
                DefaultEditor::new().map_err(|e| format!("Failed to create editor: {}", e))?;
            action_add_cosigner(state, &mut editor).await
        }
        MenuAction::SignTransaction => action_sign_transaction(state).await,
        MenuAction::FinalizePendingTransaction => action_finalize_pending_transaction(state).await,
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
    let mut state = match startup().await {
        Ok(s) => s,
        Err(e) => {
            print_error(&format!("Startup failed: {}", e));
            std::process::exit(1);
        }
    };

    let mut editor = DefaultEditor::new().expect("Failed to create editor");

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
                if let Err(e) = handle_action(action, &mut state).await {
                    print_error(&e);
                }
            }
            None => handle_invalid_choice(),
        }
    }
}
