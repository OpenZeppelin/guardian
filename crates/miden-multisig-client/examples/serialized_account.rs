//! Emit a deterministic serialized guarded-multisig account for the TS SDK's
//! round-trip tests (`packages/miden-multisig-client/tests/account-roundtrip.test.ts`).
//!
//! The TS test deserializes the account with the real WASM SDK and asserts the
//! `AccountInspector` accessors return the exact configuration written here,
//! proving the Rust writer and TS reader agree on the storage layout and that
//! the readers work against real SDK storage semantics.
//!
//! Run with:
//! ```sh
//! cargo run --example serialized_account -p miden-multisig-client
//! ```

use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{Felt, Word};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct SerializedAccountOutput {
    account_hex: String,
    threshold: u32,
    signer_commitments: Vec<String>,
    guardian_commitment: String,
}

fn mock_commitment(seed: u64) -> Word {
    Word::from([
        Felt::new_unchecked(seed),
        Felt::new_unchecked(seed + 1),
        Felt::new_unchecked(seed + 2),
        Felt::new_unchecked(seed + 3),
    ])
}

/// Hex in the SDK's `Word::toHex` format: little-endian bytes per felt.
fn word_to_sdk_hex(word: &Word) -> String {
    format!("0x{}", hex::encode(word.to_bytes()))
}

fn main() {
    let signers = vec![
        mock_commitment(1),
        mock_commitment(100),
        mock_commitment(200),
    ];
    let guardian = mock_commitment(1000);

    let config = MultisigGuardianConfig::new(2, signers.clone(), guardian);
    let account = MultisigGuardianBuilder::new(config)
        .with_seed([42u8; 32])
        .build()
        .expect("failed to build account");

    let output = SerializedAccountOutput {
        account_hex: format!("0x{}", hex::encode(account.to_bytes())),
        threshold: 2,
        signer_commitments: signers.iter().map(word_to_sdk_hex).collect(),
        guardian_commitment: word_to_sdk_hex(&guardian),
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("serialized account json serialization")
    );
}
