//! Shared helpers for the HTTP and gRPC lookup integration tests.
//!
//! The two test suites cover the same security and behavior properties at
//! different transports. Helpers live here so the suites stay byte-for-byte
//! aligned without copy-paste.

use crate::metadata::auth::Auth;
use crate::metadata::{AccountMetadata, NetworkConfig};
use crate::state::AppState;
use crate::state_object::StateObject;
use crate::testing::helpers::{TestEcdsaSigner, TestSigner};

use guardian_shared::hex::FromHex;
use guardian_shared::lookup_auth_message::LookupAuthMessage;
use miden_protocol::Word;
use miden_protocol::account::AccountId;

pub fn falcon_account(account_id: &str, cosigner_commitments: Vec<String>) -> AccountMetadata {
    AccountMetadata {
        account_id: account_id.to_string(),
        auth: Auth::MidenFalconRpo {
            cosigner_commitments,
        },
        network_config: NetworkConfig::miden_default(),
        created_at: "2024-01-01T00:00:00Z".to_string(),
        updated_at: "2024-01-01T00:00:00Z".to_string(),
        has_pending_candidate: false,
        paused_at: None,
        paused_reason: None,
        released_at: None,
    }
}

pub fn ecdsa_account(account_id: &str, cosigner_commitments: Vec<String>) -> AccountMetadata {
    AccountMetadata {
        account_id: account_id.to_string(),
        auth: Auth::MidenEcdsa {
            cosigner_commitments,
        },
        network_config: NetworkConfig::miden_default(),
        created_at: "2024-01-01T00:00:00Z".to_string(),
        updated_at: "2024-01-01T00:00:00Z".to_string(),
        has_pending_candidate: false,
        paused_at: None,
        paused_reason: None,
        released_at: None,
    }
}

pub fn evm_account(account_id: &str, signers: Vec<String>) -> AccountMetadata {
    AccountMetadata {
        account_id: account_id.to_string(),
        auth: Auth::EvmEcdsa { signers },
        network_config: NetworkConfig::miden_default(),
        created_at: "2024-01-01T00:00:00Z".to_string(),
        updated_at: "2024-01-01T00:00:00Z".to_string(),
        has_pending_candidate: false,
        paused_at: None,
        paused_reason: None,
        released_at: None,
    }
}

/// A configured account: its metadata and a stored state.
pub async fn seed(state: &AppState, metadata: AccountMetadata) {
    let account_id = metadata.account_id.clone();
    seed_unfinished(state, metadata).await;
    state
        .storage
        .submit_state(&StateObject {
            account_id,
            state_json: serde_json::json!({}),
            commitment: format!("0x{}", "11".repeat(32)),
            nonce: None,
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            auth_scheme: String::new(),
        })
        .await
        .expect("seed storage.submit_state succeeds");
}

/// A first configuration that claimed the account and failed before
/// writing its state: metadata only.
pub async fn seed_unfinished(state: &AppState, metadata: AccountMetadata) {
    state
        .metadata
        .set(metadata)
        .await
        .expect("seed metadata.set succeeds");
}

pub fn lookup_digest(key_commitment_hex: &str, timestamp_ms: i64) -> Word {
    let key_commitment_word =
        Word::from_hex(key_commitment_hex).expect("test key_commitment must be valid hex");
    LookupAuthMessage::new(timestamp_ms, key_commitment_word).to_word()
}

pub fn sign_lookup(signer: &TestSigner, key_commitment_hex: &str, timestamp_ms: i64) -> String {
    signer.sign_word(lookup_digest(key_commitment_hex, timestamp_ms))
}

pub fn sign_lookup_ecdsa(
    signer: &TestEcdsaSigner,
    key_commitment_hex: &str,
    timestamp_ms: i64,
) -> String {
    signer.sign_word(lookup_digest(key_commitment_hex, timestamp_ms))
}

pub fn sign_lookup_eip712(
    signer: &TestEcdsaSigner,
    key_commitment_hex: &str,
    timestamp_ms: i64,
) -> String {
    let lookup_hash = lookup_digest(key_commitment_hex, timestamp_ms);
    signer.sign_prehash(guardian_shared::auth_request_eip712::lookup_digest(
        lookup_hash,
    ))
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn fresh_account_id_hex(seed_byte: u8) -> String {
    AccountId::dummy(
        [seed_byte; 15],
        miden_protocol::account::AccountIdVersion::Version1,
        miden_protocol::account::AccountType::Private,
        miden_protocol::account::AssetCallbackFlag::Disabled,
    )
    .to_hex()
}
