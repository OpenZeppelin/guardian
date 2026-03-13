use std::fs;
use std::path::PathBuf;

use miden_multisig_client::word_from_hex;
use miden_protocol::crypto::rand::{FeltRng, RpoRandomCoin};
use miden_protocol::Word;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct P2idSerialVector {
    name: String,
    seed: String,
    output: String,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("miden-multisig-client")
        .join("p2id-serial-vectors.json")
}

fn load_vectors() -> Vec<P2idSerialVector> {
    let fixture = fs::read_to_string(fixture_path()).expect("p2id serial vector fixture should exist");
    serde_json::from_str(&fixture).expect("p2id serial vector fixture should parse")
}

fn word_to_hex(word: &Word) -> String {
    let bytes: Vec<u8> = word
        .iter()
        .flat_map(|felt| felt.as_int().to_le_bytes())
        .collect();
    format!("0x{}", hex::encode(bytes))
}

#[test]
fn draw_word_matches_shared_p2id_serial_vectors() {
    for vector in load_vectors() {
        let seed = word_from_hex(&vector.seed)
            .unwrap_or_else(|err| panic!("vector '{}' has invalid seed: {}", vector.name, err));
        let mut rng = RpoRandomCoin::new(seed);
        let actual = word_to_hex(&rng.draw_word());

        assert_eq!(actual, vector.output, "vector '{}'", vector.name);
    }
}
