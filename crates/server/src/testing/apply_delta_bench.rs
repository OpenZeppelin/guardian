//! Release-mode micro-benchmark for [`MidenNetworkClient::apply_delta`], the
//! synchronous, CPU-bound state-reconstruction section used by every
//! canonicalization candidate and every `push_delta` request. Production calls
//! run this work on the blocking pool behind a process-wide CPU bound.
//!
//! For an existing account, `apply_delta` deserializes the complete snapshot;
//! Miden rebuilds each storage map's sparse Merkle tree from its serialized
//! entries. Applying the new entries then updates that in-memory tree, and
//! serialization walks the complete map again. Those load/save phases make the
//! end-to-end cost grow with account storage size. The dominant growth term is
//! the `executed_transactions` replay map, which gains an entry per transaction
//! and is never pruned. Cosigner count adds a smaller term.
//!
//! Three payload shapes are measured, matching the accepted-input envelope:
//! - partial multisig delta over accounts from minimal to worst-case size
//!   ([`apply_delta_cost_by_account_size`]),
//! - full-state deployment delta ([`apply_delta_cost_full_state_deployment`]),
//! - a partial delta whose transaction summary approaches the 1 MB HTTP body
//!   limit ([`apply_delta_cost_large_delta`]).
//!
//! Not a CI regression gate: `#[ignore]`, run explicitly in release with output:
//!
//! ```text
//! cargo test -p guardian-server --lib --features e2e --release \
//!     -- --ignored --nocapture apply_delta_cost
//! ```

use crate::network::miden::MidenNetworkClient;
use crate::network::{NetworkClient, NetworkType};
use guardian_shared::ToJson;
use miden_client::Word;
use miden_client::account::Account;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::delta::AccountVaultDelta;
use miden_protocol::account::{AccountDelta, AccountStoragePatch, StorageMapKey, StorageSlotName};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::transaction::{
    InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
};
use miden_protocol::{Felt, Word as MidenWord, ZERO};
use std::hint::black_box;
use std::time::Instant;

const EXECUTED_TXS_SLOT: &str = "openzeppelin::multisig::executed_transactions";

const WARMUP_ITERS: usize = 25;
const MEASURED_ITERS: usize = 200;

/// `(cosigners, executed_transactions)` pairs spanning realistic to worst-case
/// account sizes. The replay map dominates cost, so it is swept widest.
const ACCOUNT_SIZE_CONFIGS: &[(usize, usize)] = &[
    (3, 0),
    (3, 1_000),
    (11, 10_000),
    (50, 10_000),
    (50, 50_000),
    (50, 100_000),
];

/// Target size for the "largest accepted" delta: just under the 1 MB HTTP body
/// limit enforced on `push_delta`.
const LARGE_DELTA_TARGET_BYTES: usize = 950_000;

/// Build an *existing* multisig-guardian account with `cosigners` signers and a
/// replay map pre-loaded with `executed_txs` synthetic entries, standing in for
/// that many prior transactions. `build_existing` (not `build`) matches what
/// canonicalization actually reconstructs: an account already on chain with a
/// nonce and history, whose ID is not re-derived from the seed at
/// `Account::from_json`, so seeding the storage map afterwards stays valid.
fn build_account(cosigners: usize, executed_txs: usize) -> Account {
    let signer_commitments: Vec<Word> = (0..cosigners)
        .map(|_| SecretKey::new().public_key().to_commitment())
        .collect();
    let guardian_commitment = SecretKey::new().public_key().to_commitment();
    let config = MultisigGuardianConfig::new(2, signer_commitments, guardian_commitment);

    let mut account = MultisigGuardianBuilder::new(config)
        .with_seed([0xab; 32])
        .build_existing()
        .expect("build existing multisig-guardian account");

    let executed_txs_name = StorageSlotName::new(EXECUTED_TXS_SLOT).expect("valid slot name");
    for i in 0..executed_txs {
        seed_replay_entry(&mut account, &executed_txs_name, i as u64);
    }
    account
}

fn seed_replay_entry(account: &mut Account, slot: &StorageSlotName, i: u64) {
    let key = MidenWord::from([
        Felt::new_unchecked(i),
        Felt::new_unchecked(0x5eed),
        ZERO,
        ZERO,
    ]);
    account
        .storage_mut()
        .set_map_item(
            slot,
            StorageMapKey::new(key),
            MidenWord::from([Felt::new_unchecked(1), ZERO, ZERO, ZERO]),
        )
        .expect("seed replay-map entry");
}

/// A partial delta with `entries` new replay-map inserts plus a nonce bump. One
/// insert is the per-transaction minimum; many inserts model the largest delta
/// the body limit accepts.
fn build_partial_delta(account: &Account, entries: usize) -> serde_json::Value {
    let executed_txs_name = StorageSlotName::new(EXECUTED_TXS_SLOT).expect("valid slot name");
    let map_entries = (0..entries.max(1)).map(|i| {
        let key = MidenWord::from([
            Felt::new_unchecked(0xdead_0000 + i as u64),
            Felt::new_unchecked(0xbeef),
            ZERO,
            ZERO,
        ]);
        (
            StorageMapKey::new(key),
            MidenWord::from([Felt::new_unchecked(1), ZERO, ZERO, ZERO]),
        )
    });
    let storage_patch = AccountStoragePatch::builder()
        .update_map(executed_txs_name, map_entries)
        .build();

    let delta = AccountDelta::new(
        account.id(),
        storage_patch,
        AccountVaultDelta::default(),
        None,
        Felt::new_unchecked(1),
    )
    .expect("build account delta");

    tx_summary_json(delta)
}

/// A full-state (deployment) delta: the account's code plus its nonce bump,
/// which `apply_delta` reconstructs through `Account::try_from` rather than the
/// deserialize-and-apply path.
fn build_full_state_delta(account: &Account) -> serde_json::Value {
    let delta = AccountDelta::new(
        account.id(),
        AccountStoragePatch::default(),
        AccountVaultDelta::default(),
        Some(account.code().clone()),
        Felt::new_unchecked(1),
    )
    .expect("build account delta");
    assert!(delta.is_full_state(), "delta must be a full-state delta");
    tx_summary_json(delta)
}

fn tx_summary_json(delta: AccountDelta) -> serde_json::Value {
    TransactionSummary::new(
        delta,
        InputNotes::new(Vec::new()).expect("input notes"),
        RawOutputNotes::new(Vec::new()).expect("output notes"),
        MidenWord::from([ZERO; 4]),
        0,
        TransactionSummaryUserParams::new([ZERO; 7]),
    )
    .to_json()
}

fn json_bytes(value: &serde_json::Value) -> usize {
    serde_json::to_vec(value).expect("serialize json").len()
}

/// Number of replay-map inserts whose delta payload lands just under
/// `target_bytes`, derived from the measured per-entry cost rather than guessed.
fn entries_for_delta_target(account: &Account, target_bytes: usize) -> usize {
    let base = json_bytes(&build_partial_delta(account, 1));
    let with_probe = json_bytes(&build_partial_delta(account, 101));
    let per_entry = (with_probe - base) / 100;
    target_bytes.saturating_sub(base) / per_entry.max(1)
}

fn percentile(sorted_micros: &[u128], p: f64) -> u128 {
    if sorted_micros.is_empty() {
        return 0;
    }
    let rank = (p * (sorted_micros.len() - 1) as f64).round() as usize;
    sorted_micros[rank]
}

fn fmt_micros(micros: u128) -> String {
    if micros >= 1_000 {
        format!("{:.3} ms", micros as f64 / 1_000.0)
    } else {
        format!("{micros} µs")
    }
}

/// Warm up, then time `apply_delta` `MEASURED_ITERS` times, returning the
/// per-call durations in microseconds, sorted ascending.
fn measure(
    client: &MidenNetworkClient,
    state_json: &serde_json::Value,
    delta_payload: &serde_json::Value,
) -> Vec<u128> {
    client
        .apply_delta(state_json, delta_payload)
        .expect("apply_delta must succeed on the benchmark fixture");

    for _ in 0..WARMUP_ITERS {
        let out = client
            .apply_delta(black_box(state_json), black_box(delta_payload))
            .expect("apply_delta");
        black_box(out);
    }

    let mut samples = Vec::with_capacity(MEASURED_ITERS);
    for _ in 0..MEASURED_ITERS {
        let started = Instant::now();
        let out = client
            .apply_delta(black_box(state_json), black_box(delta_payload))
            .expect("apply_delta");
        samples.push(started.elapsed().as_micros());
        black_box(out);
    }
    samples.sort_unstable();
    samples
}

fn print_header(first_col: &str) {
    println!(
        "{first_col:>12} {:>12} {:>11} {:>12} {:>12} {:>12} {:>12} {:>12}",
        "state_bytes", "delta_B", "min", "p50", "p95", "p99", "max"
    );
}

fn print_row(label: String, state_bytes: usize, delta_bytes: usize, samples: &[u128]) {
    println!(
        "{label:>12} {state_bytes:>12} {delta_bytes:>11} {:>12} {:>12} {:>12} {:>12} {:>12}",
        fmt_micros(samples[0]),
        fmt_micros(percentile(samples, 0.50)),
        fmt_micros(percentile(samples, 0.95)),
        fmt_micros(percentile(samples, 0.99)),
        fmt_micros(samples[samples.len() - 1]),
    );
}

#[tokio::test]
#[ignore = "release-mode benchmark; run with --release --ignored --nocapture"]
async fn apply_delta_cost_by_account_size() {
    let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenTestnet);
    println!(
        "\napply_delta — partial delta by account size (warmup={WARMUP_ITERS}, measured={MEASURED_ITERS})"
    );
    print_header("cosig/txs");

    for &(cosigners, executed_txs) in ACCOUNT_SIZE_CONFIGS {
        let account = build_account(cosigners, executed_txs);
        let state_json = account.to_json();
        let delta_payload = build_partial_delta(&account, 1);
        let samples = measure(&client, &state_json, &delta_payload);
        print_row(
            format!("{cosigners}/{executed_txs}"),
            json_bytes(&state_json),
            json_bytes(&delta_payload),
            &samples,
        );
    }
    println!();
}

#[tokio::test]
#[ignore = "release-mode benchmark; run with --release --ignored --nocapture"]
async fn apply_delta_cost_full_state_deployment() {
    let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenTestnet);
    println!(
        "\napply_delta — full-state deployment (warmup={WARMUP_ITERS}, measured={MEASURED_ITERS})"
    );
    print_header("cosigners");

    let empty_prev_state = serde_json::json!({});
    for &cosigners in &[3usize, 50] {
        let account = build_account(cosigners, 0);
        let delta_payload = build_full_state_delta(&account);
        let samples = measure(&client, &empty_prev_state, &delta_payload);
        print_row(
            cosigners.to_string(),
            json_bytes(&empty_prev_state),
            json_bytes(&delta_payload),
            &samples,
        );
    }
    println!();
}

#[tokio::test]
#[ignore = "release-mode benchmark; run with --release --ignored --nocapture"]
async fn apply_delta_cost_large_delta() {
    let client = MidenNetworkClient::lazy_for_test(NetworkType::MidenTestnet);
    println!(
        "\napply_delta — largest accepted delta (~1 MB body limit) (warmup={WARMUP_ITERS}, measured={MEASURED_ITERS})"
    );
    print_header("base txs");

    for &base_executed_txs in &[0usize, 10_000] {
        let account = build_account(3, base_executed_txs);
        let entries = entries_for_delta_target(&account, LARGE_DELTA_TARGET_BYTES);
        let delta_payload = build_partial_delta(&account, entries);
        let state_json = account.to_json();
        let samples = measure(&client, &state_json, &delta_payload);
        print_row(
            base_executed_txs.to_string(),
            json_bytes(&state_json),
            json_bytes(&delta_payload),
            &samples,
        );
    }
    println!();
}
