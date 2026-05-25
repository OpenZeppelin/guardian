//! Payload fixtures for `delta_summary` unit tests.
//!
//! See `research.md` Decision 10 for the two on-disk shapes covered.
//! Each fixture is paired with a documented expected outcome in the
//! decoder / classifier tests that consume it.

use guardian_shared::ToJson;
use miden_protocol::Felt;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::account::delta::{AccountDelta, AccountStorageDelta, AccountVaultDelta};
use miden_protocol::transaction::{InputNotes, RawOutputNotes, TransactionSummary};
use miden_protocol::ZERO;
use serde_json::{Value, json};

/// Stable test account id used for the empty TransactionSummary
/// constructed in [`empty_tx_summary_json`]. Matches the format used
/// across the server crate's other test fixtures.
const TEST_ACCOUNT_ID_HEX: &str = "0x7bfb0f38b0fafa103f86a805594170";

/// Build the JSON shape Miden uses for `tx_summary` — `{ "data": base64 }`
/// containing an empty (no notes, no vault delta, no storage delta)
/// TransactionSummary. Used as the `tx_summary` inner value in every
/// wrapper fixture and as the entire payload in
/// [`push_delta_raw_tx_summary`].
fn empty_tx_summary_json() -> Value {
    let account_id = AccountId::from_hex(TEST_ACCOUNT_ID_HEX).expect("valid fixture account id");
    let delta = AccountDelta::new(
        account_id,
        AccountStorageDelta::default(),
        AccountVaultDelta::default(),
        Felt::ZERO,
    )
    .expect("empty AccountDelta");

    let tx_summary = TransactionSummary::new(
        delta,
        InputNotes::new(Vec::new()).expect("empty input notes"),
        RawOutputNotes::new(Vec::new()).expect("empty output notes"),
        Word::from([ZERO; 4]),
    );

    tx_summary.to_json()
}

// --- Multisig wrapper fixtures ---------------------------------------

/// `metadata.proposal_type = "p2id"` with recipient/faucet/amount.
/// Classifier expects `(AssetTransfer, Some("p2id"), summary_with_asset_and_counterparty)`.
pub fn multisig_p2id_wrapper() -> Value {
    json!({
        "tx_summary": empty_tx_summary_json(),
        "metadata": {
            "proposal_type": "p2id",
            "recipient_id": "0xrecipient0000000000000000000001",
            "faucet_id": "0xfaucet000000000000000000000001",
            "amount": "100",
        },
        "signatures": [],
    })
}

/// `metadata.proposal_type = "add_signer"`. Classifier expects
/// `(AccountStorageChange, Some("add_signer"), summary_without_asset)`.
pub fn multisig_add_signer() -> Value {
    json!({
        "tx_summary": empty_tx_summary_json(),
        "metadata": {
            "proposal_type": "add_signer",
            "target_threshold": 2,
            "signer_commitments": ["0xabc123"],
        },
        "signatures": [],
    })
}

/// `metadata.proposal_type = "switch_guardian"`. Classifier expects
/// `(GuardianSwitch, Some("switch_guardian"), _)`.
pub fn multisig_switch_guardian() -> Value {
    json!({
        "tx_summary": empty_tx_summary_json(),
        "metadata": {
            "proposal_type": "switch_guardian",
            "new_guardian_pubkey": "0xdef456",
            "new_guardian_endpoint": "https://new-guardian.example",
        },
        "signatures": [],
    })
}

// --- Raw push_delta fixture ------------------------------------------

/// Raw `{data: base64}` TransactionSummary (no wrapper). Classifier
/// expects `(AccountStorageChange, None, _)` because the summary is
/// empty so the topology heuristic falls into the "no notes, only
/// account-delta state" bucket per FR-002b.
pub fn push_delta_raw_tx_summary() -> Value {
    empty_tx_summary_json()
}

// --- Opaque / malformed fixtures -------------------------------------

/// EVM-style payload that has no `tx_summary` and no top-level `data`.
/// Resolver returns `Opaque { reason: "unrecognized_payload_shape" }`;
/// classifier returns `Custom`.
pub fn evm_placeholder() -> Value {
    json!({
        "evm_payload": "0xfeedfacedeadbeef",
        "chain_id": 1,
    })
}

/// Wrapper shape with `tx_summary.data` set to a non-base64 string.
/// Resolver returns `Opaque { reason: "malformed_base64" }`; classifier
/// returns `Custom`.
pub fn malformed_base64() -> Value {
    json!({
        "tx_summary": { "data": "not valid base64!!!" },
        "metadata": {
            "proposal_type": "p2id",
        },
    })
}

/// All fixtures, for property-style assertions ("every payload
/// classifies to a non-null category").
pub fn all_fixtures() -> Vec<Value> {
    vec![
        multisig_p2id_wrapper(),
        multisig_add_signer(),
        multisig_switch_guardian(),
        push_delta_raw_tx_summary(),
        evm_placeholder(),
        malformed_base64(),
    ]
}
