//! Cross-language parity fixtures for session grants, session logout and
//! revoke-all messages, and P-256 session keys.
//!
//! Running this test under `GUARDIAN_REGEN_SESSION_FIXTURES=1` rewrites
//! `tests/fixtures/session_grant_vectors.json` from the canonical Rust
//! implementation. Without that env var set, the test asserts the on-disk
//! fixture matches the current Rust output — the cross-language parity gate
//! between this crate and the TypeScript port in
//! `packages/miden-multisig-client`, whose tests also check every EIP-712
//! digest against viem.
//!
//! Regenerate after any intentional change to a digest layout or domain tag,
//! then update the TypeScript port and its tests in lockstep.

use guardian_shared::auth_request_eip712::{revoke_all_digest, session_digest};
use guardian_shared::session_grant::{
    SESSION_GRANT_SCOPE, SessionGrant, SessionLogoutMessage, SessionRevokeAllMessage,
    session_domain_tag, session_logout_domain_tag, session_revoke_all_domain_tag,
};
use guardian_shared::session_key::SessionKey;
use miden_protocol::Word;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct GrantVector {
    name: String,
    signer_commitment_hex: String,
    session_public_key_hex: String,
    origin: String,
    issued_at: u64,
    expires_at: u64,
    guardian_commitment_hex: String,
    network: String,
    expected_expires: String,
    expected_digest_hex: String,
    expected_eip712_digest_hex: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct LogoutVector {
    name: String,
    session_public_key_hex: String,
    timestamp_ms: i64,
    expected_digest_hex: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RevokeAllVector {
    name: String,
    signer_commitment_hex: String,
    timestamp_ms: i64,
    expected_digest_hex: String,
    expected_eip712_digest_hex: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SessionKeyVector {
    name: String,
    secret_hex: String,
    expected_public_key_hex: String,
    message_hex: String,
    signature_hex: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Fixture {
    schema: String,
    scope: String,
    domain_tag_hex: String,
    logout_domain_tag_hex: String,
    revoke_all_domain_tag_hex: String,
    grants: Vec<GrantVector>,
    logouts: Vec<LogoutVector>,
    revoke_alls: Vec<RevokeAllVector>,
    session_keys: Vec<SessionKeyVector>,
}

const SCHEMA: &str = "guardian.session_grant.v1";

fn word_to_hex(word: Word) -> String {
    format!("0x{}", hex::encode(word.as_bytes()))
}

fn session_key(seed: u8) -> SessionKey {
    SessionKey::from_bytes(&[seed; 32]).expect("valid scalar")
}

/// (name, signer commitment, session key seed, origin, issued_at, expires_at,
/// Guardian commitment, network)
type GrantInput = (
    &'static str,
    Word,
    u8,
    &'static str,
    u64,
    u64,
    Word,
    &'static str,
);

fn build_fixture() -> Fixture {
    let grant_inputs: Vec<GrantInput> = vec![
        (
            "devnet_8h",
            Word::from([1u32, 2, 3, 4]),
            1,
            "https://multisig.miden.xyz",
            1_791_280_800,
            1_791_309_600,
            Word::from([5u32, 6, 7, 8]),
            "devnet",
        ),
        (
            "testnet_1h",
            Word::from([0xdeadbeefu32, 0xcafebabe, 0x12345678, 0x87654321]),
            2,
            "https://wallet.example:8443",
            1_704_067_200,
            1_704_070_800,
            Word::from([0xaau32, 0xbb, 0xcc, 0xdd]),
            "testnet",
        ),
        (
            "local_leap_day",
            Word::from([0u32, 0, 0, 0]),
            3,
            "",
            951_696_000,
            951_782_400,
            Word::from([0xffffffffu32, 0xffffffff, 0xffffffff, 0xffffffff]),
            "local",
        ),
        (
            "unicode_network",
            Word::from([0x80000000u32, 0x80000000, 0x80000000, 0x80000000]),
            4,
            "https://xn--r8jz45g.jp",
            1_800_000_000,
            1_800_000_001,
            Word::from([9u32, 9, 9, 9]),
            "dévnet 测试",
        ),
        (
            "far_future",
            Word::from([7u32, 7, 7, 7]),
            5,
            "http://localhost:3000",
            4_102_358_400,
            4_102_444_799,
            Word::from([3u32, 1, 4, 1]),
            "devnet",
        ),
    ];

    let grants = grant_inputs
        .into_iter()
        .map(
            |(name, signer, key_seed, origin, issued_at, expires_at, guardian_key, network)| {
                let session_public_key = session_key(key_seed).public_key();
                let grant = SessionGrant::new(
                    signer,
                    &session_public_key,
                    origin,
                    issued_at,
                    expires_at,
                    guardian_key,
                    network,
                )
                .expect("valid grant");
                GrantVector {
                    name: name.to_string(),
                    signer_commitment_hex: word_to_hex(signer),
                    session_public_key_hex: format!("0x{}", hex::encode(session_public_key)),
                    origin: origin.to_string(),
                    issued_at,
                    expires_at,
                    guardian_commitment_hex: word_to_hex(guardian_key),
                    network: network.to_string(),
                    expected_expires: grant.expires(),
                    expected_digest_hex: word_to_hex(grant.to_word()),
                    expected_eip712_digest_hex: format!(
                        "0x{}",
                        hex::encode(session_digest(&grant))
                    ),
                }
            },
        )
        .collect();

    let logouts = [
        ("zero_timestamp", 1u8, 0i64),
        ("typical", 2, 1_791_280_800_000),
        ("negative_timestamp", 3, -1),
    ]
    .into_iter()
    .map(|(name, key_seed, timestamp_ms)| {
        let public_key = session_key(key_seed).public_key();
        LogoutVector {
            name: name.to_string(),
            session_public_key_hex: format!("0x{}", hex::encode(public_key)),
            timestamp_ms,
            expected_digest_hex: word_to_hex(
                SessionLogoutMessage::new(&public_key, timestamp_ms).to_word(),
            ),
        }
    })
    .collect();

    let revoke_alls = [
        ("zero_timestamp", Word::from([1u32, 2, 3, 4]), 0i64),
        (
            "typical",
            Word::from([0xdeadbeefu32, 0xcafebabe, 0x12345678, 0x87654321]),
            1_791_280_800_000,
        ),
    ]
    .into_iter()
    .map(|(name, signer, timestamp_ms)| {
        let message = SessionRevokeAllMessage::new(signer, timestamp_ms);
        RevokeAllVector {
            name: name.to_string(),
            signer_commitment_hex: word_to_hex(signer),
            timestamp_ms,
            expected_digest_hex: word_to_hex(message.to_word()),
            expected_eip712_digest_hex: format!(
                "0x{}",
                hex::encode(revoke_all_digest(&message).unwrap())
            ),
        }
    })
    .collect();

    let session_keys = [
        ("seed_01", 1u8, Word::from([1u32, 2, 3, 4])),
        (
            "seed_7f",
            0x7f,
            Word::from([0xffffffffu32, 0, 0xffffffff, 0]),
        ),
    ]
    .into_iter()
    .map(|(name, seed, message)| {
        let key = session_key(seed);
        SessionKeyVector {
            name: name.to_string(),
            secret_hex: format!("0x{}", hex::encode([seed; 32])),
            expected_public_key_hex: key.public_key_hex(),
            message_hex: word_to_hex(message),
            signature_hex: key.sign_hex(message),
        }
    })
    .collect();

    Fixture {
        schema: SCHEMA.to_string(),
        scope: SESSION_GRANT_SCOPE.to_string(),
        domain_tag_hex: word_to_hex(session_domain_tag()),
        logout_domain_tag_hex: word_to_hex(session_logout_domain_tag()),
        revoke_all_domain_tag_hex: word_to_hex(session_revoke_all_domain_tag()),
        grants,
        logouts,
        revoke_alls,
        session_keys,
    }
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("session_grant_vectors.json")
}

#[test]
fn session_grant_vectors_match_fixture() {
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
        "session_grant_vectors.json is stale; regenerate with GUARDIAN_REGEN_SESSION_FIXTURES=1 \
         and update the TypeScript port in lockstep"
    );
}

#[test]
fn session_key_vectors_verify() {
    for vector in build_fixture().session_keys {
        let public_key = hex::decode(vector.expected_public_key_hex.trim_start_matches("0x"))
            .expect("public key hex");
        let message = hex::decode(vector.message_hex.trim_start_matches("0x")).expect("message");
        let message = Word::try_from(message.as_slice()).expect("message word");
        let signature =
            hex::decode(vector.signature_hex.trim_start_matches("0x")).expect("signature hex");
        guardian_shared::session_key::verify(&public_key, message, &signature)
            .unwrap_or_else(|e| panic!("{}: {e}", vector.name));
    }
}
