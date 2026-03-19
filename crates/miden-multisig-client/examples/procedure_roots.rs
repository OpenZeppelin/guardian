//! Extract and display procedure roots for multisig accounts.
//!
//! This example builds a test multisig account and prints all procedure roots,
//! which are deterministic based on the compiled MASM bytecode.
//!
//! Run with:
//! ```sh
//! cargo run --example procedure_roots
//! cargo run --example procedure_roots -- --json
//! ```

use std::env;

use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
use miden_protocol::{Felt, Word};
use miden_standards::account::wallets::BasicWallet;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct ProcedureRootRecord {
    name: &'static str,
    component: &'static str,
    index: usize,
    rust_hex: String,
    typescript_hex: String,
}

#[derive(Debug, Serialize)]
struct ProcedureRootOutput {
    component_order: Vec<&'static str>,
    procedure_roots: Vec<ProcedureRootRecord>,
}

fn word_to_rust_hex(word: &Word) -> String {
    word.iter()
        .rev()
        .map(|felt| format!("{:016x}", felt.as_int()))
        .collect::<Vec<_>>()
        .join("")
}

fn word_to_typescript_hex(word: &Word) -> String {
    let bytes: Vec<u8> = word
        .iter()
        .flat_map(|felt| felt.as_int().to_le_bytes())
        .collect();
    hex::encode(bytes)
}

fn procedure_name_and_component(
    idx: usize,
    root_word: Word,
    receive_asset: Word,
    send_asset: Word,
) -> (&'static str, &'static str) {
    if root_word == receive_asset {
        ("receive_asset", "BasicWallet")
    } else if root_word == send_asset {
        ("send_asset", "BasicWallet")
    } else {
        match idx {
            0 => ("update_signers", "Multisig"),
            1 => ("update_procedure_threshold", "Multisig"),
            2 => ("update_psm", "Multisig"),
            3 => ("auth_tx", "Multisig"),
            4 => ("verify_psm", "PSM"),
            _ => ("unknown", "unknown"),
        }
    }
}

fn mock_commitment(seed: u64) -> Word {
    Word::from([
        Felt::new(seed),
        Felt::new(seed + 1),
        Felt::new(seed + 2),
        Felt::new(seed + 3),
    ])
}

fn main() {
    let receive_asset = BasicWallet::receive_asset_digest();
    let send_asset = BasicWallet::move_asset_to_note_digest();

    let config = MultisigPsmConfig::new(1, vec![mock_commitment(1)], mock_commitment(10));
    let account = MultisigPsmBuilder::new(config)
        .with_seed([42u8; 32])
        .build()
        .expect("Failed to build account");

    let procedure_roots: Vec<ProcedureRootRecord> = account
        .code()
        .procedures()
        .iter()
        .enumerate()
        .map(|(idx, procedure)| {
            let root_word: Word = *procedure.mast_root();
            let (name, component) =
                procedure_name_and_component(idx, root_word, receive_asset, send_asset);

            ProcedureRootRecord {
                name,
                component,
                index: idx,
                rust_hex: format!("0x{}", word_to_rust_hex(&root_word)),
                typescript_hex: format!("0x{}", word_to_typescript_hex(&root_word)),
            }
        })
        .collect();

    if env::args().any(|arg| arg == "--json") {
        let output = ProcedureRootOutput {
            component_order: vec!["Multisig + PSM (auth)", "BasicWallet"],
            procedure_roots,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output).expect("procedure root json serialization")
        );
        return;
    }

    println!("\n=== PROCEDURE ROOTS ===\n");
    println!("BasicWallet procedures (from miden_standards):");
    println!("  receive_asset: {}", procedure_roots[6].rust_hex);
    println!("  send_asset:    {}", procedure_roots[5].rust_hex);

    println!("\nAll account procedures (ordered by component):");
    println!("  Component order: Multisig + PSM (auth) -> BasicWallet\n");

    for procedure in &procedure_roots {
        println!("  [{}] {}", procedure.index, procedure.rust_hex);
        println!("      -> {} ({})", procedure.name, procedure.component);
    }

    println!("\n=== RUST CONSTANTS (procedures.rs) ===\n");
    for procedure in &procedure_roots {
        println!("  {}: '{}',", procedure.name, procedure.rust_hex);
    }

    println!("\n=== TYPESCRIPT CONSTANTS (procedures.ts) ===\n");
    for procedure in &procedure_roots {
        println!("  {}: '{}',", procedure.name, procedure.typescript_hex);
    }

    println!("\n=== END ===\n");
}
