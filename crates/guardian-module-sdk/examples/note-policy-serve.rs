//! The `note-policy` module served over gRPC, for the Guardian to call as a remote module.
//! Walkthrough in `docs/guides/policy-modules/custom.md`.
//!
//! ```text
//! cargo run -p guardian-module-sdk --example note-policy-serve -- \
//!   --listen 127.0.0.1:7444 --instance note-policy <token-file>
//! ```

mod note_policy;

use std::net::SocketAddr;

use guardian_module_sdk::serve::serve;
use note_policy::NotePolicy;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut listen: SocketAddr = "127.0.0.1:7444".parse()?;
    let mut server = serve(NotePolicy);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => listen = args.next().ok_or("--listen needs an address")?.parse()?,
            "--instance" => {
                let id = args.next().ok_or("--instance needs an id")?;
                let token_file = args.next().ok_or("--instance needs a token file")?;
                server = server.instance(id, std::fs::read_to_string(token_file)?.trim());
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    println!("note-policy listening on {listen}");
    server.run(listen).await?;
    Ok(())
}
