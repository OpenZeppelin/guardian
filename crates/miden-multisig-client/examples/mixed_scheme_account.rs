//! Emit a deterministic guarded-multisig account whose approvers use different signature
//! schemes, for the TS SDK's per-approver scheme tests
//! (`packages/miden-multisig-client/tests/mixed-scheme-account.test.ts`).
//!
//! The account is built straight from the upstream `AuthGuardedMultisig` component, so the TS
//! test can check both directions against `miden-standards` itself: its readers recover each
//! approver's scheme from this account, and its writer produces the same storage for the same
//! configuration.
//!
//! Run with:
//! ```sh
//! cargo run --example mixed_scheme_account -p miden-multisig-client
//! ```

use miden_protocol::account::auth::{AuthScheme, PublicKeyCommitment};
use miden_protocol::account::{AccountBuilder, AccountComponent};
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::{
    Approver, ApproverSet, AuthGuardedMultisig, AuthGuardedMultisigConfig, GuardianConfig,
};
use miden_standards::account::wallets::BasicWallet;
use serde::Serialize;

const SEED: [u8; 32] = [43u8; 32];
const THRESHOLD: u32 = 2;

#[derive(Debug, Serialize)]
struct SignerOutput {
    commitment: String,
    scheme: &'static str,
}

#[derive(Debug, Serialize)]
struct MixedSchemeAccountOutput {
    account_hex: String,
    seed_hex: String,
    threshold: u32,
    signers: Vec<SignerOutput>,
    guardian_commitment: String,
    guardian_scheme: &'static str,
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

fn scheme_name(scheme: AuthScheme) -> &'static str {
    match scheme {
        AuthScheme::Falcon512Poseidon2 => "falcon",
        AuthScheme::EcdsaK256Keccak => "ecdsa",
        _ => panic!("unexpected auth scheme"),
    }
}

fn main() {
    let approvers = vec![
        Approver::new(
            PublicKeyCommitment::from(mock_commitment(1)),
            AuthScheme::Falcon512Poseidon2,
        ),
        Approver::new(
            PublicKeyCommitment::from(mock_commitment(100)),
            AuthScheme::EcdsaK256Keccak,
        ),
        Approver::new(
            PublicKeyCommitment::from(mock_commitment(200)),
            AuthScheme::Falcon512Poseidon2,
        ),
    ];
    let guardian = Approver::new(
        PublicKeyCommitment::from(mock_commitment(1000)),
        AuthScheme::EcdsaK256Keccak,
    );

    let approver_set = ApproverSet::new(approvers.clone(), THRESHOLD).expect("valid approver set");
    let config = AuthGuardedMultisigConfig::new(approver_set, GuardianConfig::new(guardian))
        .expect("valid guarded-multisig config");
    let component: AccountComponent = AuthGuardedMultisig::new(config)
        .expect("valid guarded multisig")
        .into();

    let account = AccountBuilder::new(SEED)
        .with_component(component)
        .with_component(BasicWallet)
        .build()
        .expect("failed to build account");

    let output = MixedSchemeAccountOutput {
        account_hex: format!("0x{}", hex::encode(account.to_bytes())),
        seed_hex: format!("0x{}", hex::encode(SEED)),
        threshold: THRESHOLD,
        signers: approvers
            .iter()
            .map(|approver| SignerOutput {
                commitment: word_to_sdk_hex(&Word::from(approver.pub_key())),
                scheme: scheme_name(approver.auth_scheme()),
            })
            .collect(),
        guardian_commitment: word_to_sdk_hex(&Word::from(guardian.pub_key())),
        guardian_scheme: scheme_name(guardian.auth_scheme()),
    };

    println!(
        "{}",
        serde_json::to_string_pretty(&output).expect("mixed scheme account json serialization")
    );
}
