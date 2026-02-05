//! Extract and display procedure roots for multisig accounts.
//!
//! This example builds a test multisig account and prints all procedure roots,
//! which are deterministic based on the compiled MASM bytecode.
//!
//! Run with:
//! ```sh
//! cargo run --example procedure_roots
//! ```

use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
use miden_lib::account::wallets::BasicWallet;
use miden_objects::{Felt, Word};
use private_state_manager_shared::SignatureScheme;

fn main() {
    // Helper to format Word as hex (big-endian)
    fn word_to_hex(word: &Word) -> String {
        word.iter()
            .rev()
            .map(|felt| format!("{:016x}", felt.as_int()))
            .collect::<Vec<_>>()
            .join("")
    }

    // Create a mock commitment for testing
    fn mock_commitment(seed: u64) -> Word {
        Word::from([
            Felt::new(seed),
            Felt::new(seed + 1),
            Felt::new(seed + 2),
            Felt::new(seed + 3),
        ])
    }

    let scheme = std::env::var("PSM_SIGNATURE_SCHEME").unwrap_or_else(|_| "falcon".to_string());
    let signature_scheme = match scheme.as_str() {
        "ecdsa" => SignatureScheme::Ecdsa,
        _ => SignatureScheme::Falcon,
    };

    println!("\n=== PROCEDURE ROOTS ({}) ===\n", scheme);

    // BasicWallet procedure roots (compile-time constants from miden_lib)
    let receive_asset = BasicWallet::receive_asset_digest();
    let send_asset = BasicWallet::move_asset_to_note_digest();

    println!("BasicWallet procedures (from miden_lib):");
    println!("  receive_asset: 0x{}", word_to_hex(&receive_asset));
    println!("  send_asset:    0x{}", word_to_hex(&send_asset));

    // Build a test account to extract component procedure roots
    let config = MultisigPsmConfig::new(1, vec![mock_commitment(1)], mock_commitment(10))
        .with_signature_scheme(signature_scheme);

    let account = MultisigPsmBuilder::new(config)
        .with_seed([42u8; 32])
        .build()
        .expect("Failed to build account");

    // Get all procedures from account code
    println!("\nAll account procedures (ordered by component):");
    println!("  Component order: Multisig (auth) -> PSM -> BasicWallet\n");

    let code = account.code();
    for (idx, procedure) in code.procedures().iter().enumerate() {
        let root = procedure.mast_root();
        let root_word: Word = *root;

        // Match against known BasicWallet roots to identify them
        let name = if root_word == receive_asset {
            "receive_asset (BasicWallet)"
        } else if root_word == send_asset {
            "send_asset (BasicWallet)"
        } else {
            match idx {
                0 => "update_signers (Multisig)",
                1 => "auth_tx (Multisig)",
                2 => "update_psm (PSM)",
                3 => "verify_psm (PSM)",
                _ => "unknown",
            }
        };

        println!("  [{}] 0x{}", idx, word_to_hex(&root_word));
        println!("      -> {}", name);
    }

    println!("\n=== TYPESCRIPT/RUST CONSTANTS ===\n");
    println!("// Copy these to procedures.ts / procedures.rs if roots change:");
    println!();

    for (idx, procedure) in code.procedures().iter().enumerate() {
        let root = procedure.mast_root();
        let root_word: Word = *root;
        let hex = word_to_hex(&root_word);

        let name = if root_word == receive_asset {
            "receive_asset"
        } else if root_word == send_asset {
            "send_asset"
        } else {
            match idx {
                0 => "update_signers",
                1 => "auth_tx",
                2 => "update_psm",
                3 => "verify_psm",
                _ => "unknown",
            }
        };

        println!("  {}: '0x{}',", name, hex);
    }

    println!("\n=== END ===\n");
}
