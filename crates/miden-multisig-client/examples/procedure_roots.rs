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

use miden_protocol::Word;
use miden_standards::account::wallets::BasicWallet;
use miden_standards::code_builder::CodeBuilder;
use serde::Serialize;

/// The auth component MASM this package bundles and builds accounts from.
///
/// Deliberately NOT `miden_standards::account::auth::AuthGuardedMultisig::code()`:
/// `auth_tx` calls `miden::standards::fee`, so its root moves with how the standards
/// package is linked, and that component is linked statically while `CodeBuilder` links
/// dynamically. Both SDK builders produce the dynamic build, so the roots printed here
/// describe every guardian account. The module docs on `crate::procedures` carry the
/// mechanism and are the authority this must not be allowed to disagree with.
const PACKAGE_AUTH_MASM: &str = include_str!(
    "../../../packages/miden-multisig-client/masm/account_components/auth/guarded_multisig.masm"
);

#[derive(Debug, Serialize)]
struct ProcedureRootRecord {
    name: String,
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
        .map(|felt| format!("{:016x}", felt.as_canonical_u64()))
        .collect::<Vec<_>>()
        .join("")
}

fn word_to_typescript_hex(word: &Word) -> String {
    let bytes: Vec<u8> = word
        .iter()
        .flat_map(|felt| felt.as_canonical_u64().to_le_bytes())
        .collect();
    hex::encode(bytes)
}

fn record(
    index: usize,
    name: String,
    component: &'static str,
    root_word: Word,
) -> ProcedureRootRecord {
    ProcedureRootRecord {
        name,
        component,
        index,
        rust_hex: format!("0x{}", word_to_rust_hex(&root_word)),
        typescript_hex: format!("0x{}", word_to_typescript_hex(&root_word)),
    }
}

fn main() {
    // Compile the bundled MASM against the standards this build resolves, so the
    // pin describes the account that will actually be created.
    let auth_code = CodeBuilder::new()
        .compile_component_code("guarded_multisig", PACKAGE_AUTH_MASM)
        .expect("bundled guarded-multisig MASM should compile");
    let auth_root = |masm_name: &str| -> Word {
        let export = auth_code
            .exports()
            .find(|e| e.path.to_string().rsplit("::").next() == Some(masm_name))
            .unwrap_or_else(|| panic!("upstream procedure `{masm_name}` not found"));
        auth_code
            .get_procedure_root_by_path(&*export.path)
            .expect("auth procedure root by path")
            .into()
    };

    let auth_procedures = [
        ("update_signers", "update_signers_and_threshold"),
        ("update_procedure_threshold", "set_procedure_threshold"),
        ("auth_tx", "auth_tx_guarded_multisig"),
        ("update_guardian", "update_guardian_public_key"),
    ];

    let mut procedure_roots: Vec<ProcedureRootRecord> = auth_procedures
        .iter()
        .enumerate()
        .map(|(index, (facing, masm))| {
            record(
                index,
                facing.to_string(),
                "Multisig + GUARDIAN (auth)",
                auth_root(masm),
            )
        })
        .collect();

    let next = procedure_roots.len();
    let send_asset: Word = BasicWallet::move_asset_to_note_root().into();
    let receive_asset: Word = BasicWallet::receive_asset_root().into();
    procedure_roots.push(record(
        next,
        "send_asset".to_string(),
        "BasicWallet",
        send_asset,
    ));
    procedure_roots.push(record(
        next + 1,
        "receive_asset".to_string(),
        "BasicWallet",
        receive_asset,
    ));

    if env::args().any(|arg| arg == "--json") {
        let output = ProcedureRootOutput {
            component_order: vec!["Multisig + GUARDIAN (auth)", "BasicWallet"],
            procedure_roots,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output).expect("procedure root json serialization")
        );
        return;
    }

    println!("\n=== PROCEDURE ROOTS ===\n");
    println!("\nAll account procedures (ordered by component):");
    println!("  Component order: Multisig + GUARDIAN (auth) -> BasicWallet\n");

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
