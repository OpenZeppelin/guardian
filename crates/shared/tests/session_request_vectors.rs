//! Cross-language parity fixtures for version 2 session request envelopes.
//!
//! Running this test under `GUARDIAN_REGEN_SESSION_FIXTURES=1` rewrites
//! `tests/fixtures/session_request_vectors.json` from the canonical Rust
//! implementation. Without that env var set, the test asserts the on-disk
//! fixture matches the current Rust output. The TypeScript ports in
//! `packages/guardian-client` and `packages/miden-multisig-client` read the
//! same file.

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::session_key::{SessionKey, verify_bytes};
use guardian_shared::session_request::{
    SESSION_REQUEST_AUTH_FORMAT, SESSION_REQUEST_VERSION, SessionOperation, SessionRequestEnvelope,
    SessionTransport,
};
use miden_protocol::account::{AccountId, AccountIdVersion, AccountType, AssetCallbackFlag};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const SCHEMA: &str = "guardian.session_request.v2";
const SECRET_SEED: u8 = 0x2a;
const TIMESTAMP_MS: i64 = 1_767_225_600_000;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Fixture {
    schema: String,
    auth_format: String,
    version: u16,
    secret_hex: String,
    public_key_hex: String,
    operations: Vec<OperationEntry>,
    vectors: Vec<RequestVector>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct OperationEntry {
    id: u16,
    name: String,
    replay_policy: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RequestVector {
    name: String,
    account_id_hex: String,
    timestamp_ms: i64,
    payload_hex: String,
    transport: u8,
    operation: u16,
    request_word_hex: String,
    envelope_hex: String,
    signed_digest_hex: String,
    record_key_hex: String,
    signature_hex: String,
}

fn hex0x(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn account_id() -> AccountId {
    AccountId::dummy(
        [0x8a; 15],
        AccountIdVersion::Version1,
        AccountType::Private,
        AssetCallbackFlag::Disabled,
    )
}

fn vector(
    key: &SessionKey,
    name: &str,
    payload: &[u8],
    transport: SessionTransport,
    operation: SessionOperation,
) -> RequestVector {
    let message = AuthRequestMessage::new(
        account_id(),
        TIMESTAMP_MS,
        AuthRequestPayload::from_bytes(payload),
    );
    let envelope = SessionRequestEnvelope::new(transport, operation, &message);
    RequestVector {
        name: name.to_string(),
        account_id_hex: account_id().to_hex(),
        timestamp_ms: TIMESTAMP_MS,
        payload_hex: hex0x(payload),
        transport: transport.id(),
        operation: operation.id(),
        request_word_hex: hex0x(message.to_word().as_bytes()),
        envelope_hex: hex0x(envelope.to_bytes()),
        signed_digest_hex: hex0x(envelope.signed_digest()),
        record_key_hex: hex0x(envelope.record_key(&key.public_key())),
        signature_hex: key.sign_bytes_hex(&envelope.to_bytes()),
    }
}

fn build_fixture() -> Fixture {
    let key = SessionKey::from_bytes(&[SECRET_SEED; 32]).expect("valid scalar");
    let account_query = format!(r#"{{"account_id":"{}"}}"#, account_id().to_hex());
    let mut vectors = Vec::new();
    for operation in SessionOperation::ALL {
        for (transport, label) in [
            (SessionTransport::Http, "http"),
            (SessionTransport::Grpc, "grpc"),
        ] {
            vectors.push(vector(
                &key,
                &format!("{}_{label}", operation.name()),
                account_query.as_bytes(),
                transport,
                operation,
            ));
        }
    }
    vectors.push(vector(
        &key,
        "empty_payload",
        &[],
        SessionTransport::Http,
        SessionOperation::GetState,
    ));

    Fixture {
        schema: SCHEMA.to_string(),
        auth_format: SESSION_REQUEST_AUTH_FORMAT.to_string(),
        version: SESSION_REQUEST_VERSION,
        secret_hex: hex0x([SECRET_SEED; 32]),
        public_key_hex: key.public_key_hex(),
        operations: SessionOperation::ALL
            .into_iter()
            .map(|operation| OperationEntry {
                id: operation.id(),
                name: operation.name().to_string(),
                replay_policy: format!("{:?}", operation.replay_policy()).to_lowercase(),
            })
            .collect(),
        vectors,
    }
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("session_request_vectors.json")
}

#[test]
fn session_request_vectors_match_fixture() {
    let fixture = build_fixture();
    let path = fixture_path();

    if std::env::var("GUARDIAN_REGEN_SESSION_FIXTURES").is_ok() {
        let json = serde_json::to_string_pretty(&fixture).expect("serialize fixture");
        std::fs::write(&path, format!("{json}\n")).expect("write fixture");
        return;
    }

    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing {}: {e}. Run with GUARDIAN_REGEN_SESSION_FIXTURES=1 to create it.",
            path.display()
        )
    });
    let on_disk: Fixture = serde_json::from_str(&on_disk).expect("parse fixture");
    assert_eq!(
        on_disk, fixture,
        "session_request_vectors.json is stale; regenerate with \
         GUARDIAN_REGEN_SESSION_FIXTURES=1 and update the TypeScript ports in lockstep"
    );
}

#[test]
fn each_signature_verifies_only_for_its_own_transport_and_operation() {
    let fixture = build_fixture();
    let public_key = hex::decode(fixture.public_key_hex.trim_start_matches("0x")).unwrap();
    let decode = |value: &str| hex::decode(value.trim_start_matches("0x")).unwrap();
    for signed in &fixture.vectors {
        let signature = decode(&signed.signature_hex);
        for presented in &fixture.vectors {
            let accepted =
                verify_bytes(&public_key, &decode(&presented.envelope_hex), &signature).is_ok();
            assert_eq!(
                accepted,
                signed.envelope_hex == presented.envelope_hex,
                "signature for {} presented as {}",
                signed.name,
                presented.name
            );
        }
    }
}

#[test]
fn same_payload_operations_produce_distinct_envelopes() {
    let fixture = build_fixture();
    let get_state = &fixture.vectors[0];
    let get_nonce = &fixture.vectors[2];
    assert_eq!(get_state.request_word_hex, get_nonce.request_word_hex);
    assert_ne!(get_state.envelope_hex, get_nonce.envelope_hex);
}

#[test]
fn each_single_field_mutation_fails_verification() {
    let key = SessionKey::from_bytes(&[SECRET_SEED; 32]).expect("valid scalar");
    let payload = format!(r#"{{"account_id":"{}"}}"#, account_id().to_hex());
    let envelope = |account: AccountId, timestamp: i64, payload: &[u8], operation| {
        SessionRequestEnvelope::new(
            SessionTransport::Http,
            operation,
            &AuthRequestMessage::new(account, timestamp, AuthRequestPayload::from_bytes(payload)),
        )
        .to_bytes()
        .to_vec()
    };
    let signed = envelope(
        account_id(),
        TIMESTAMP_MS,
        payload.as_bytes(),
        SessionOperation::GetState,
    );
    let signature = hex::decode(key.sign_bytes_hex(&signed).trim_start_matches("0x")).unwrap();
    let public_key = key.public_key();
    assert!(verify_bytes(&public_key, &signed, &signature).is_ok());

    let other_account = AccountId::dummy(
        [0x8b; 15],
        AccountIdVersion::Version1,
        AccountType::Private,
        AssetCallbackFlag::Disabled,
    );
    let mut other_version = signed.clone();
    other_version[26] ^= 0x01;
    let mut other_transport = signed.clone();
    other_transport[27] = SessionTransport::Grpc.id();
    let mutations = [
        ("version", other_version),
        ("transport", other_transport),
        (
            "operation",
            envelope(
                account_id(),
                TIMESTAMP_MS,
                payload.as_bytes(),
                SessionOperation::GetCanonicalNonce,
            ),
        ),
        (
            "account",
            envelope(
                other_account,
                TIMESTAMP_MS,
                payload.as_bytes(),
                SessionOperation::GetState,
            ),
        ),
        (
            "timestamp",
            envelope(
                account_id(),
                TIMESTAMP_MS + 1,
                payload.as_bytes(),
                SessionOperation::GetState,
            ),
        ),
        (
            "payload",
            envelope(
                account_id(),
                TIMESTAMP_MS,
                b"{}",
                SessionOperation::GetState,
            ),
        ),
    ];
    for (field, mutated) in mutations {
        assert_ne!(mutated, signed, "{field} changes the envelope");
        assert!(
            verify_bytes(&public_key, &mutated, &signature).is_err(),
            "a signature does not verify with another {field}"
        );
    }
}

#[test]
fn both_s_forms_of_one_signature_are_one_request() {
    use p256::ecdsa::Signature;

    let fixture = build_fixture();
    let vector = &fixture.vectors[0];
    let decode = |value: &str| hex::decode(value.trim_start_matches("0x")).unwrap();
    let public_key = decode(&fixture.public_key_hex);
    let envelope = decode(&vector.envelope_hex);
    let signature = Signature::from_slice(&decode(&vector.signature_hex)).unwrap();
    let flipped = Signature::from_scalars(signature.r(), -*signature.s()).unwrap();
    assert_ne!(signature.to_bytes(), flipped.to_bytes());

    for form in [signature, flipped] {
        assert!(verify_bytes(&public_key, &envelope, &form.to_bytes()).is_ok());
    }
    let key = SessionKey::from_bytes(&[SECRET_SEED; 32]).unwrap();
    let message = AuthRequestMessage::new(
        account_id(),
        TIMESTAMP_MS,
        AuthRequestPayload::from_bytes(&decode(&vector.payload_hex)),
    );
    let record_key =
        SessionRequestEnvelope::new(SessionTransport::Http, SessionOperation::GetState, &message)
            .record_key(&key.public_key());
    assert_eq!(
        hex0x(record_key),
        vector.record_key_hex,
        "the record key does not depend on the signature, so both forms share one record"
    );
}
