use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use guardian_shared::multisig_threshold::ThresholdedTransaction;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_multisig_client::{MultisigAccount, ProcedureName};
use miden_protocol::Word;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ThresholdVector {
    name: String,
    threshold: u32,
    overrides: BTreeMap<String, u32>,
    expected: BTreeMap<String, u32>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("miden-multisig-client")
        .join("threshold-vectors.json")
}

fn load_vectors() -> Vec<ThresholdVector> {
    let fixture =
        fs::read_to_string(fixture_path()).expect("threshold vector fixture should exist");
    serde_json::from_str(&fixture).expect("threshold vector fixture should parse")
}

fn account(vector: &ThresholdVector) -> MultisigAccount {
    let signers = (1..=3u32).map(|v| Word::from([v, 0, 0, 0])).collect();
    let overrides = vector
        .overrides
        .iter()
        .map(|(procedure, threshold)| {
            let procedure: ProcedureName = procedure.parse().expect("known procedure");
            (procedure.root(), *threshold)
        })
        .collect();
    let config =
        MultisigGuardianConfig::new(vector.threshold, signers, Word::from([99u32, 0, 0, 0]))
            .with_proc_threshold_overrides(overrides);
    let account = MultisigGuardianBuilder::new(config)
        .with_seed([7u8; 32])
        .build()
        .unwrap_or_else(|error| panic!("{}: account builds: {error}", vector.name));
    MultisigAccount::new(account)
}

#[test]
fn effective_thresholds_match_the_shared_vectors() {
    for vector in load_vectors() {
        let account = account(&vector);
        for (proposal_type, expected) in &vector.expected {
            let transaction = ThresholdedTransaction::of_proposal_type(Some(proposal_type));
            assert_eq!(
                account
                    .effective_threshold_for(transaction)
                    .expect("threshold"),
                *expected,
                "{}: {proposal_type}",
                vector.name
            );
        }
    }
}
