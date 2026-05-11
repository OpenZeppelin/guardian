//! Cross-language parity fixtures for `LookupAuthMessage`.
//!
//! Running this test under `GUARDIAN_REGEN_LOOKUP_FIXTURES=1` rewrites
//! `tests/fixtures/lookup_auth_vectors.json` from the canonical Rust
//! implementation. Without that env var set, the test asserts the on-disk
//! fixture matches the current Rust output — the cross-language parity gate
//! that prevents silent signing-contract drift between the Rust crate and the
//! TypeScript port (see `packages/guardian-client` / `packages/miden-multisig-client`).
//!
//! Regenerate after any intentional change to the digest layout or the domain
//! tag, then update the TypeScript port and its tests in lockstep.

use guardian_shared::lookup_auth_message::{LookupAuthMessage, lookup_domain_tag};
use miden_protocol::Word;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Vector {
    name: &'static str,
    timestamp_ms: i64,
    key_commitment_hex: String,
    expected_digest_hex: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Fixture {
    schema: &'static str,
    domain_tag_bytes: &'static str,
    domain_tag_hex: String,
    vectors: Vec<Vector>,
}

const SCHEMA: &str = "guardian.lookup_auth_message.v1";
const DOMAIN_TAG_BYTES: &str = "guardian.lookup.v1";

fn word_from_u32x4(words: [u32; 4]) -> Word {
    Word::from(words)
}

fn word_to_hex(word: Word) -> String {
    format!("0x{}", hex::encode(word.as_bytes()))
}

/// Builds the canonical set of vectors. Adding cases here is the way to extend
/// parity coverage; the TypeScript suite re-reads the same JSON file.
fn build_vectors() -> Vec<(&'static str, i64, Word)> {
    vec![
        ("zero_timestamp", 0, word_from_u32x4([0, 0, 0, 0])),
        (
            "zero_timestamp_nonzero_commitment",
            0,
            word_from_u32x4([1, 2, 3, 4]),
        ),
        (
            "epoch_2026_min",
            1_704_067_200_000,
            word_from_u32x4([0xdeadbeef, 0xcafebabe, 0x12345678, 0x87654321]),
        ),
        (
            "epoch_2026_typical",
            1_714_939_200_000,
            word_from_u32x4([0xaa, 0xbb, 0xcc, 0xdd]),
        ),
        (
            "max_i64_timestamp",
            i64::MAX,
            word_from_u32x4([0xffffffff, 0xffffffff, 0xffffffff, 0xffffffff]),
        ),
        (
            "negative_timestamp",
            -1,
            word_from_u32x4([0x11, 0x22, 0x33, 0x44]),
        ),
        (
            "commitment_with_high_bits",
            1_700_000_000_000,
            word_from_u32x4([0x80000000, 0x80000000, 0x80000000, 0x80000000]),
        ),
        (
            "sequential_commitment",
            1_700_000_000_000,
            word_from_u32x4([1, 1, 1, 1]),
        ),
    ]
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("lookup_auth_vectors.json")
}

fn current_fixture() -> Fixture {
    let domain_tag = lookup_domain_tag();
    let vectors = build_vectors()
        .into_iter()
        .map(|(name, timestamp_ms, key_commitment)| {
            let digest = LookupAuthMessage::new(timestamp_ms, key_commitment).to_word();
            Vector {
                name,
                timestamp_ms,
                key_commitment_hex: word_to_hex(key_commitment),
                expected_digest_hex: word_to_hex(digest),
            }
        })
        .collect();
    Fixture {
        schema: SCHEMA,
        domain_tag_bytes: DOMAIN_TAG_BYTES,
        domain_tag_hex: word_to_hex(domain_tag),
        vectors,
    }
}

#[test]
fn fixture_matches_or_regenerates() {
    let fixture = current_fixture();
    let json = serde_json::to_string_pretty(&fixture).expect("fixture must serialize") + "\n";
    let path = fixture_path();

    if std::env::var("GUARDIAN_REGEN_LOOKUP_FIXTURES").is_ok() {
        std::fs::create_dir_all(path.parent().expect("fixture parent dir"))
            .expect("create fixture dir");
        std::fs::write(&path, &json).expect("write fixture");
        eprintln!("Regenerated {}", path.display());
        return;
    }

    let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "lookup_auth_vectors.json missing or unreadable at {}: {err}.\n\
             Run `GUARDIAN_REGEN_LOOKUP_FIXTURES=1 cargo test -p guardian-shared --test lookup_auth_vectors` to bootstrap it.",
            path.display(),
        );
    });

    assert_eq!(
        on_disk, json,
        "lookup_auth_vectors.json drifted from the Rust implementation. If this is intentional, \
         regenerate with `GUARDIAN_REGEN_LOOKUP_FIXTURES=1 cargo test -p guardian-shared --test lookup_auth_vectors` \
         and update the TypeScript port + its parity tests in the same change."
    );
}
