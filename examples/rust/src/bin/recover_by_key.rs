//! Account-recovery-by-key example: given only a Falcon signing key,
//! resolve the account ID(s) it authorizes and fetch their state.
//!
//! Prerequisite: an account whose authorization set contains the key's
//! commitment must already exist on the running GUARDIAN server. Run
//! `cargo run --bin guardian-rust-example` first.
//!
//! Usage:
//! ```text
//! cargo run --bin recover_by_key -- \
//!   --guardian http://localhost:50051 \
//!   --secret-key-hex 0x<hex of FalconSecretKey serialization>
//! ```

use std::sync::Arc;

use clap::Parser;
use miden_client::Serializable;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::Deserializable;

use guardian_client::{ClientResult, FalconKeyStore, GuardianClient};

#[derive(Parser, Debug)]
#[command(name = "recover-by-key")]
#[command(about = "GUARDIAN account recovery by key")]
struct Args {
    /// GUARDIAN gRPC endpoint URL.
    #[arg(long, default_value = "http://localhost:50051")]
    guardian: String,

    /// Hex-encoded Falcon SecretKey bytes. `0x` prefix is optional.
    /// In a production wallet this is re-derived from the user's seed,
    /// never passed on the command line.
    #[arg(long)]
    secret_key_hex: String,
}

fn parse_secret_key(hex: &str) -> Result<SecretKey, String> {
    let trimmed = hex.trim_start_matches("0x").trim_start_matches("0X");
    let bytes =
        ::hex::decode(trimmed).map_err(|e| format!("--secret-key-hex is not valid hex: {e}"))?;
    SecretKey::read_from_bytes(&bytes).map_err(|e| {
        format!(
            "--secret-key-hex did not deserialize as a Falcon SecretKey ({} bytes): {e}",
            bytes.len()
        )
    })
}

#[tokio::main]
async fn main() -> ClientResult<()> {
    let args = Args::parse();

    println!("=== GUARDIAN account recovery by key ===\n");

    println!("Step 1: Load signing key");
    let secret_key = match parse_secret_key(&args.secret_key_hex) {
        Ok(key) => key,
        Err(err) => {
            eprintln!("  ✗ {err}");
            std::process::exit(2);
        }
    };
    let public_key = secret_key.public_key();
    let commitment = public_key.to_commitment();
    let commitment_hex = format!("0x{}", ::hex::encode(commitment.to_bytes()));
    println!("  ✓ Key commitment: {commitment_hex}\n");

    println!("Step 2: Connect to GUARDIAN");
    let signer = Arc::new(FalconKeyStore::new(secret_key));
    let mut client = match GuardianClient::connect(args.guardian.clone()).await {
        Ok(c) => c.with_signer(signer),
        Err(e) => {
            eprintln!("  ✗ Failed to connect to {}: {e}", args.guardian);
            eprintln!(
                "  Hint: start GUARDIAN locally with: \
                  cargo run --package guardian-server --bin server"
            );
            std::process::exit(1);
        }
    };
    println!("  ✓ Connected to {}\n", args.guardian);

    // Proof-of-possession auth is handled by the client; the caller does
    // not need to think about the auth digest.
    println!("Step 3: Look up accounts by key commitment");
    let lookup = match client
        .lookup_account_by_key_commitment(&commitment_hex)
        .await
    {
        Ok(response) => response,
        Err(e) => {
            eprintln!("  ✗ Lookup failed: {e}");
            std::process::exit(1);
        }
    };
    if lookup.accounts.is_empty() {
        println!("  ⚠ No account on this Guardian operator authorizes this commitment.");
        return Ok(());
    }
    println!(
        "  ✓ Found {} account{}:",
        lookup.accounts.len(),
        if lookup.accounts.len() == 1 { "" } else { "s" }
    );
    for entry in &lookup.accounts {
        println!("      • {}", entry.account_id);
    }
    println!();

    println!("Step 4: Fetch current state for each recovered account");
    for entry in &lookup.accounts {
        match miden_protocol::account::AccountId::from_hex(&entry.account_id) {
            Ok(account_id) => match client.get_state(&account_id).await {
                Ok(response) => {
                    if let Some(state) = response.state {
                        println!("  ✓ {}: commitment {}", state.account_id, state.commitment);
                    } else {
                        println!(
                            "  ⚠ {}: state envelope returned but state is empty",
                            entry.account_id
                        );
                    }
                }
                Err(e) => {
                    println!("  ✗ {}: get_state failed: {e}", entry.account_id);
                }
            },
            Err(e) => {
                println!(
                    "  ✗ Could not parse account_id '{}' returned by lookup: {e}",
                    entry.account_id
                );
            }
        }
    }

    println!("\n=== Recovery complete ===");
    Ok(())
}
