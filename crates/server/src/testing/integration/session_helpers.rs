//! Shared helpers for the HTTP and gRPC session integration tests.

use crate::coordination::InMemorySessionStore;
use crate::session::{MidenSessions, SessionConfig};
use crate::state::AppState;
use crate::testing::helpers::{TestEcdsaSigner, TestSigner};
use crate::testing::integration::lookup_helpers;

use guardian_shared::SignatureScheme;
use guardian_shared::auth_request_eip712::revoke_all_digest;
use guardian_shared::hex::FromHex;
use guardian_shared::session_grant::{SessionGrant, SessionLogoutMessage, SessionRevokeAllMessage};
use guardian_shared::session_key::SessionKey;
use miden_protocol::Word;
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// A shorter maximum lifetime than the default, so tests can exceed it.
pub const MAX_TTL_SECONDS: u32 = 3600;

/// Caps the session lifetime at [`MAX_TTL_SECONDS`].
pub fn with_max_ttl(mut state: AppState) -> AppState {
    state.miden_sessions = Arc::new(MidenSessions::new(
        SessionConfig::new(MAX_TTL_SECONDS).unwrap(),
        Arc::new(InMemorySessionStore::new()),
    ));
    state
}

/// Makes `commitment` a cosigner of a fresh account, so it may hold sessions.
pub async fn seed_cosigner(state: &AppState, commitment: &str, scheme: SignatureScheme) {
    static NEXT: AtomicU8 = AtomicU8::new(0xa0);
    let account_id = lookup_helpers::fresh_account_id_hex(NEXT.fetch_add(1, Ordering::Relaxed));
    let metadata = match scheme {
        SignatureScheme::Falcon => {
            lookup_helpers::falcon_account(&account_id, vec![commitment.to_string()])
        }
        SignatureScheme::Ecdsa => {
            lookup_helpers::ecdsa_account(&account_id, vec![commitment.to_string()])
        }
    };
    lookup_helpers::seed(state, metadata).await;
}

/// Grant values as a client sends them; tests override one field at a time.
pub struct GrantInput {
    pub signer_commitment: String,
    pub origin: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub guardian_commitment: String,
    pub network: String,
}

impl GrantInput {
    /// A grant without an origin, as a client outside a browser makes.
    pub fn for_state(state: &AppState, signer_commitment: &str, scheme: SignatureScheme) -> Self {
        let now = state.clock.now().timestamp() as u64;
        Self {
            signer_commitment: signer_commitment.to_string(),
            origin: String::new(),
            issued_at: now,
            expires_at: now + 600,
            guardian_commitment: state.ack.commitment(&scheme),
            network: state.dashboard.environment().to_string(),
        }
    }

    pub fn grant(&self, session_key: &SessionKey) -> SessionGrant {
        SessionGrant::new(
            Word::from_hex(&self.signer_commitment).unwrap(),
            &session_key.public_key(),
            self.origin.as_str(),
            self.issued_at,
            self.expires_at,
            Word::from_hex(&self.guardian_commitment).unwrap(),
            self.network.as_str(),
        )
        .unwrap()
    }

    /// `POST /session` body.
    pub fn body(&self, session_key: &SessionKey, scheme: &str, signature: &str) -> Value {
        json!({
            "scheme": scheme,
            "signature": signature,
            "grant": {
                "signer_commitment": self.signer_commitment,
                "session_public_key": session_key.public_key_hex(),
                "origin": self.origin,
                "issued_at": self.issued_at,
                "expires_at": self.expires_at,
                "guardian_commitment": self.guardian_commitment,
                "network": self.network,
            }
        })
    }
}

/// `(x-signature, x-timestamp)` of a session-key-signed logout.
pub fn logout_signature(session_key: &SessionKey) -> (String, i64) {
    let timestamp = chrono::Utc::now().timestamp_millis();
    let digest = SessionLogoutMessage::new(&session_key.public_key(), timestamp).to_word();
    (session_key.sign_hex(digest), timestamp)
}

/// The revoke-all message of `signer_commitment` at `timestamp_ms`.
pub fn revoke_all_word(signer_commitment: &str, timestamp_ms: i64) -> Word {
    SessionRevokeAllMessage::new(Word::from_hex(signer_commitment).unwrap(), timestamp_ms).to_word()
}

/// `(x-signature, x-timestamp)` of a raw Falcon wallet-signed revoke-all.
pub fn revoke_all_signature(signer: &TestSigner) -> (String, i64) {
    let timestamp = chrono::Utc::now().timestamp_millis();
    (
        signer.sign_word(revoke_all_word(&signer.commitment_hex, timestamp)),
        timestamp,
    )
}

/// `(x-signature, x-timestamp)` of an EIP-712 ECDSA wallet-signed revoke-all.
pub fn revoke_all_eip712_signature(signer: &TestEcdsaSigner) -> (String, i64) {
    let timestamp = chrono::Utc::now().timestamp_millis();
    let signer_commitment = Word::from_hex(&signer.commitment_hex).unwrap();
    let message = SessionRevokeAllMessage::new(signer_commitment, timestamp);
    (signer.sign_prehash(revoke_all_digest(&message)), timestamp)
}
