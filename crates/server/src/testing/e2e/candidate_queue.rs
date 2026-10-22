//! End-to-end coverage of the per-account candidate queue (issue #17)
//! over the real Miden delta-application path, driven by the fixture
//! chain `account.json` → `queue_1` → `queue_2` → `queue_3` (threshold-only
//! deltas whose post-state commitments are pinned in `commitments.json`).
//! The `delta_1` → `delta_2` → `delta_3` chain adds a signer twice, and
//! nothing chains behind a candidate that changes the signer set, so it
//! drives the tests where that refusal is the point.
//!
//! `queued_chain_lands_and_canonicalizes_in_order`:
//! 1. Push the three chained deltas back-to-back, never waiting for
//!    canonicalization. The second and third are admitted against a
//!    queue tail the server reconstructs by replaying the queued
//!    payloads, and their server-computed post-states match the pinned
//!    fixture commitments — the replay is the real thing.
//! 2. A delta competing for the canonical base is refused, the depth cap
//!    refuses the delta that would exceed it, and a proposal is pinned to
//!    the queue tail rather than the canonical state.
//! 3. The chain lands (the registered on-chain commitment moves to the
//!    tail's post-state); one worker pass promotes all three in order,
//!    the stored state ends at the tail, and the flag clears.
//!
//! `abandoned_head_orphans_its_successor_and_the_chain_reconciles`:
//! 1. Two chained candidates; the client abandons the head. In one pass
//!    the worker discards it and sweeps the now-orphaned successor as
//!    `retained` (reason `orphaned`), releasing the account.
//! 2. The chain lands late anyway: the reconcile pass walks the two
//!    recoverable rows as a chain and promotes both, healing the stored
//!    state.
//!
//! `cosigner_on_the_canonical_state_is_refused_behind_a_queued_candidate`:
//! one device's candidate is queued; another cosigner, synced to the
//! canonical state, proposes with the canonical nonce + 1. The proposal
//! is refused at once, before anyone signs it, and accepted on the new
//! canonical state once the queue drains.
//!
//! `depth_one_is_the_single_candidate_gate`: at the default depth, a
//! second delta waits for the first whatever its base, and a proposal is
//! refused, exactly as before the queue existed.
//!
//! `successor_behind_a_signer_changing_candidate_is_refused_until_it_promotes`:
//! `delta_1` adds a signer. While it is queued nothing chains behind it,
//! although the queue has room: the chained `delta_2` and a proposal at
//! the next nonce are refused and nothing is stored. Once it lands and
//! promotes, which syncs the cosigner set, `delta_2` is admitted on the
//! new canonical state.
//!
//! `proposal_must_extend_the_queue_by_exactly_one_nonce`: with one
//! candidate queued, a proposal is recorded only at the tail's nonce plus
//! one, pinned to the tail; one labelled at or below the tail's nonce, or
//! past the tail's nonce plus one (a timestamp, the TypeScript SDK's
//! default through 0.18.0), is refused.

use std::sync::Arc;

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};

use crate::canonicalization::CanonicalizationConfig;
use crate::delta_object::{DeltaObject, RetainReason};
use crate::error::GuardianError;
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::services::{
    AbandonCandidateParams, AbandonState, ConfigureAccountParams, PushDeltaParams,
    PushDeltaProposalParams, abandon_candidate, configure_account, process_canonicalizations_now,
    push_delta, push_delta_proposal,
};
use crate::state::AppState;
use crate::testing::fixtures;
use crate::testing::helpers::{IntegrationMockNetworkClient, create_test_app_state};

fn falcon_credentials(
    key: &SecretKey,
    pubkey_hex: &str,
    account_id_hex: &str,
    timestamp: i64,
) -> Credentials {
    let message = AuthRequestMessage::from_account_id_hex(
        account_id_hex,
        timestamp,
        AuthRequestPayload::empty(),
    )
    .expect("valid account ID")
    .to_word();
    let signature = key.sign(message);
    let signature_hex = format!("0x{}", hex::encode(signature.to_bytes()));
    Credentials::signature(pubkey_hex.to_string(), signature_hex, timestamp)
}

struct QueueSetup {
    state: AppState,
    chain: Arc<IntegrationMockNetworkClient>,
    account_id: String,
    signer_key: SecretKey,
    signer_pubkey_hex: String,
    initial_commitment: String,
    /// Pinned post-state commitments of `delta_1..=3`, index 0 = delta 1.
    expected_commitments: Vec<String>,
    /// Pinned post-state commitments of `queue_1..=3`, index 0 = queue 1.
    expected_queue_commitments: Vec<String>,
    next_timestamp: i64,
}

/// A proposal payload carrying `delta`'s transaction summary.
fn proposal_payload(delta: &DeltaObject, description: &str) -> serde_json::Value {
    serde_json::json!({
        "tx_summary": delta.delta_payload,
        "signatures": [],
        "metadata": {
            "proposal_type": "custom",
            "description": description
        }
    })
}

impl QueueSetup {
    fn credentials(&mut self) -> Credentials {
        self.next_timestamp += 1;
        falcon_credentials(
            &self.signer_key,
            &self.signer_pubkey_hex,
            &self.account_id,
            self.next_timestamp,
        )
    }

    /// `delta_N`: the chain that adds a signer with its first two deltas.
    fn fixture_delta(&self, delta_num: u8) -> DeltaObject {
        self.delta_from_fixture(crate::testing::helpers::load_fixture_delta(delta_num))
    }

    /// `queue_N`: the threshold-only chain, which keeps the signer set and
    /// so can be queued behind itself.
    fn queue_delta(&self, delta_num: u8) -> DeltaObject {
        self.delta_from_fixture(crate::testing::helpers::load_queue_fixture_delta(delta_num))
    }

    fn delta_from_fixture(&self, fixture: serde_json::Value) -> DeltaObject {
        DeltaObject {
            account_id: self.account_id.clone(),
            nonce: fixture["nonce"].as_u64().expect("fixture nonce"),
            prev_commitment: fixture["prev_commitment"]
                .as_str()
                .expect("fixture prev_commitment")
                .to_string(),
            new_commitment: None,
            delta_payload: fixture["delta_payload"].clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: Default::default(),
            metadata: None,
        }
    }

    /// The queue fixtures raise the threshold as they apply, so the
    /// authorizing proposal carries all three fixture approvals. It is
    /// stored only for the duration of the push: these tests assert
    /// over the proposal store, and rows this harness plants would
    /// change what those assertions see.
    async fn push(&mut self, delta: DeltaObject) -> Result<DeltaObject, GuardianError> {
        let authorization = crate::testing::helpers::store_authorizing_proposal(
            &self.state,
            &delta,
            &[
                crate::testing::helpers::fixture_secret_key_n(1),
                crate::testing::helpers::fixture_secret_key_n(2),
                crate::testing::helpers::fixture_secret_key_n(3),
            ],
        )
        .await;
        let credentials = self.credentials();
        let result = push_delta(&self.state, PushDeltaParams { delta, credentials })
            .await
            .map(|result| result.delta);
        authorization.restore(&self.state).await;
        result
    }

    async fn deltas(&self) -> Vec<DeltaObject> {
        self.state
            .storage
            .pull_deltas_after(&self.account_id, 0)
            .await
            .expect("deltas readable")
    }

    async fn delta(&self, nonce: u64) -> DeltaObject {
        self.deltas()
            .await
            .into_iter()
            .find(|d| d.nonce == nonce)
            .unwrap_or_else(|| panic!("delta {nonce} stored"))
    }

    async fn stored_commitment(&self) -> String {
        self.state
            .storage
            .pull_state(&self.account_id)
            .await
            .expect("state readable")
            .commitment
    }

    async fn has_pending_candidate(&self) -> bool {
        self.state
            .metadata
            .get(&self.account_id)
            .await
            .expect("metadata readable")
            .expect("metadata present")
            .has_pending_candidate
    }
}

/// Configure the fixture account with the real Miden delta path and the
/// registered on-chain commitment pinned at its initial state.
async fn queue_setup(max_pending_candidates: usize) -> QueueSetup {
    let mut state = create_test_app_state().await;
    state.canonicalization = Some(
        CanonicalizationConfig::default()
            .with_max_pending_candidates_per_account(max_pending_candidates),
    );

    let account_json: serde_json::Value =
        serde_json::from_str(fixtures::ACCOUNT_JSON).expect("account.json parses");
    let commitments: serde_json::Value =
        serde_json::from_str(fixtures::COMMITMENTS_JSON).expect("commitments.json parses");
    let account_id = commitments["account_id"]
        .as_str()
        .expect("account_id")
        .to_string();
    let initial_commitment = commitments["initial_commitment"]
        .as_str()
        .expect("initial_commitment")
        .to_string();
    let pinned = |name: &str| -> Vec<String> {
        (1..=3)
            .map(|i| {
                commitments[format!("commitment_after_{name}_{i}")]
                    .as_str()
                    .expect("pinned commitment")
                    .to_string()
            })
            .collect()
    };
    let expected_commitments = pinned("delta");
    let expected_queue_commitments = pinned("queue");

    let keys: serde_json::Value = serde_json::from_str(fixtures::KEYS_JSON).expect("keys.json");
    // The fixture account's guardian key is the fixture guardian's: this
    // server must hold it, or every promotion would look like a switch
    // away from it and release the account.
    let guardian_key = SecretKey::read_from_bytes(
        &hex::decode(keys["guardian_secret_key"].as_str().expect("guardian key")).expect("hex"),
    )
    .expect("guardian key bytes");
    let keystore_dir =
        std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
    state.ack = crate::ack::AckRegistry::with_falcon_secret_for_tests(keystore_dir, &guardian_key)
        .await
        .expect("ack registry with the fixture guardian key");
    let secret_key_bytes =
        hex::decode(keys["signer_1_secret_key"].as_str().expect("signer key")).expect("hex");
    let signer_key = SecretKey::read_from_bytes(&secret_key_bytes).expect("signer key bytes");
    let signer_pubkey_hex = signer_key.public_key().into_hex();
    let cosigner_commitments: Vec<String> = (1..=3)
        .map(|i| {
            keys[format!("signer_{i}_commitment")]
                .as_str()
                .expect("signer commitment")
                .to_string()
        })
        .collect();

    let mut integration_client = IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    );
    integration_client.register_account(account_id.clone(), initial_commitment.clone());
    let chain = Arc::new(integration_client);
    state.network_client = chain.clone();

    let mut setup = QueueSetup {
        state,
        chain,
        account_id: account_id.clone(),
        signer_key,
        signer_pubkey_hex,
        initial_commitment,
        expected_commitments,
        expected_queue_commitments,
        next_timestamp: chrono::Utc::now().timestamp_millis(),
    };

    let credential = setup.credentials();
    configure_account(
        &setup.state,
        ConfigureAccountParams {
            account_id,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments,
            },
            network_config: NetworkConfig::miden_default(),
            initial_state: account_json,
            credential,
        },
    )
    .await
    .expect("configure_account succeeds");

    setup
}

#[tokio::test]
async fn queued_chain_lands_and_canonicalizes_in_order() {
    let mut setup = queue_setup(2).await;

    // Two chained deltas back-to-back: the second is admitted against
    // the replayed tail and its server-computed post-state is the pinned
    // fixture commitment.
    let first = setup
        .push(setup.queue_delta(1))
        .await
        .expect("delta 1 is admitted on the canonical base");
    assert!(first.status.is_candidate());
    assert_eq!(
        first.new_commitment.as_deref(),
        Some(setup.expected_queue_commitments[0].as_str())
    );
    let second = setup
        .push(setup.queue_delta(2))
        .await
        .expect("delta 2 is admitted on the queue tail without waiting");
    assert!(second.status.is_candidate());
    assert_eq!(
        second.new_commitment.as_deref(),
        Some(setup.expected_queue_commitments[1].as_str())
    );
    assert_eq!(
        setup.stored_commitment().await,
        setup.initial_commitment,
        "the canonical state does not move on admission"
    );

    // Competing for the canonical base while delta 1 holds it.
    let mut competing = setup.queue_delta(1);
    competing.nonce = 7;
    let refused = setup
        .push(competing)
        .await
        .expect_err("a delta built on the claimed canonical base is refused");
    assert!(
        matches!(refused, GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );

    // Depth 2 is full: the correctly chained third delta must wait.
    let refused = setup
        .push(setup.queue_delta(3))
        .await
        .expect_err("queue depth is enforced");
    assert!(
        matches!(refused, GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );
    // ...and a proposal is refused up front for the same reason.
    let tail_proposal_payload =
        proposal_payload(&setup.queue_delta(3), "proposed against the queue tail");
    let creds = setup.credentials();
    let refused = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 3,
            delta_payload: tail_proposal_payload.clone(),
            credentials: creds,
        },
    )
    .await
    .expect_err("a proposal is refused while the queue is full");
    assert!(matches!(refused, GuardianError::ConflictPendingDelta));

    // Raise the depth: the third delta is admitted on the replayed tail.
    setup.state.canonicalization =
        Some(CanonicalizationConfig::default().with_max_pending_candidates_per_account(3));
    let third = setup
        .push(setup.queue_delta(3))
        .await
        .expect("delta 3 is admitted once the depth allows");
    assert_eq!(
        third.new_commitment.as_deref(),
        Some(setup.expected_queue_commitments[2].as_str())
    );
    let queue = setup
        .state
        .storage
        .pull_candidate_deltas(&setup.account_id)
        .await
        .expect("queue readable");
    assert_eq!(
        queue.iter().map(|d| d.nonce).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(setup.has_pending_candidate().await);

    // A proposal is pinned to the queue tail, not the canonical state.
    setup.state.canonicalization =
        Some(CanonicalizationConfig::default().with_max_pending_candidates_per_account(4));
    let creds = setup.credentials();
    let proposal = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 4,
            delta_payload: tail_proposal_payload,
            credentials: creds,
        },
    )
    .await
    .expect("a proposal is accepted against the queue tail");
    assert_eq!(
        proposal.delta.prev_commitment, setup.expected_queue_commitments[2],
        "the proposal is pinned to the tail's post-state"
    );

    // Nothing has landed yet: the worker leaves every candidate alone
    // (the e2e worker has no grace period but never discards).
    process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    for nonce in 1..=3 {
        assert!(
            setup.delta(nonce).await.status.is_candidate(),
            "delta {nonce} waits for the chain"
        );
    }

    // The chain lands at the tail's post-state: one pass promotes the
    // whole chain in order.
    setup
        .chain
        .set_on_chain_commitment(&setup.account_id, &setup.expected_queue_commitments[2]);
    let pass = process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    assert_eq!(pass.failed_accounts, 0);
    for (index, nonce) in (1..=3).enumerate() {
        let delta = setup.delta(nonce).await;
        assert!(
            delta.status.is_canonical(),
            "delta {nonce} must be canonical, got {:?}",
            delta.status
        );
        assert_eq!(
            delta.new_commitment.as_deref(),
            Some(setup.expected_queue_commitments[index].as_str())
        );
    }
    assert_eq!(
        setup.stored_commitment().await,
        setup.expected_queue_commitments[2],
        "the stored state ends at the chain tail"
    );
    assert!(
        !setup.has_pending_candidate().await,
        "the flag clears once the queue drains"
    );
    assert!(
        setup
            .state
            .storage
            .pull_candidate_deltas(&setup.account_id)
            .await
            .expect("queue readable")
            .is_empty()
    );
}

#[tokio::test]
async fn abandoned_head_orphans_its_successor_and_the_chain_reconciles() {
    let mut setup = queue_setup(4).await;

    setup
        .push(setup.queue_delta(1))
        .await
        .expect("delta 1 is admitted");
    setup
        .push(setup.queue_delta(2))
        .await
        .expect("delta 2 is admitted on the tail");

    // The client gives up on the head of the queue.
    let creds = setup.credentials();
    let abandoned = abandon_candidate(
        &setup.state,
        AbandonCandidateParams {
            account_id: setup.account_id.clone(),
            nonce: 1,
            credentials: creds,
        },
    )
    .await
    .expect("abandon intent is recorded");
    assert_eq!(abandoned.state, AbandonState::Pending);

    // One pass: the head resolves as client-abandoned (zero quarantine)
    // and, having left the queue, orphans its successor — parked without
    // any chain observation or reconstruction from the wrong base — and
    // the account is released.
    process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    assert!(setup.delta(1).await.status.is_client_abandoned());
    let orphan = setup.delta(2).await;
    assert!(
        orphan.status.is_retained(),
        "orphaned successor must be retained, got {:?}",
        orphan.status
    );
    assert_eq!(orphan.status.retain_reason(), Some(RetainReason::Orphaned));
    assert!(!setup.has_pending_candidate().await);
    assert_eq!(setup.stored_commitment().await, setup.initial_commitment);

    // Released: a fresh delta on the canonical base is admitted again
    // (superseding the abandoned row at nonce 1)...
    let resubmitted = setup
        .push(setup.queue_delta(1))
        .await
        .expect("the account accepts new work after the sweep");
    assert!(resubmitted.status.is_candidate());
    // ...but that is not the path this test follows: drop it again via
    // abandon so the recoverable rows are exactly the original chain.
    let creds = setup.credentials();
    abandon_candidate(
        &setup.state,
        AbandonCandidateParams {
            account_id: setup.account_id.clone(),
            nonce: 1,
            credentials: creds,
        },
    )
    .await
    .expect("abandon intent is recorded");
    process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    assert!(setup.delta(1).await.status.is_client_abandoned());

    // The original chain lands late after all: the reconcile pass walks
    // both recoverable rows from the stored base to the on-chain
    // commitment and promotes them in order.
    setup
        .chain
        .set_on_chain_commitment(&setup.account_id, &setup.expected_queue_commitments[1]);
    let pass = process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    assert_eq!(pass.failed_accounts, 0);
    for (index, nonce) in (1..=2).enumerate() {
        let delta = setup.delta(nonce).await;
        assert!(
            delta.status.is_canonical(),
            "delta {nonce} must reconcile to canonical, got {:?}",
            delta.status
        );
        assert_eq!(
            delta.new_commitment.as_deref(),
            Some(setup.expected_queue_commitments[index].as_str())
        );
    }
    assert_eq!(
        setup.stored_commitment().await,
        setup.expected_queue_commitments[1]
    );
}

#[tokio::test]
async fn cosigner_on_the_canonical_state_is_refused_behind_a_queued_candidate() {
    let mut setup = queue_setup(4).await;
    let queued = setup
        .push(setup.queue_delta(1))
        .await
        .expect("device A's delta is queued");
    assert_eq!(queued.nonce, 1);

    // Cosigner B synced the canonical state (all `/state` serves), so its
    // SDK labels the proposal with the canonical nonce + 1: device A's
    // queued slot. Its push could never succeed, so it is refused before
    // anyone signs it, and nothing is stored.
    let creds = setup.credentials();
    let refused = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 1,
            delta_payload: proposal_payload(&setup.queue_delta(1), "built on the canonical state"),
            credentials: creds,
        },
    )
    .await
    .expect_err("a proposal at the queued nonce is refused");
    assert!(
        matches!(refused, GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );
    assert!(
        setup
            .state
            .storage
            .pull_pending_proposals(&setup.account_id)
            .await
            .expect("proposals readable")
            .is_empty(),
        "nothing is stored for cosigners to sign"
    );

    // The queue drains: device A's transaction lands and promotes. B
    // resyncs to the new canonical state and proposes on it.
    setup
        .chain
        .set_on_chain_commitment(&setup.account_id, &setup.expected_queue_commitments[0]);
    let pass = process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(setup.delta(1).await.status.is_canonical());
    let creds = setup.credentials();
    let accepted = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 2,
            delta_payload: proposal_payload(
                &setup.queue_delta(2),
                "built on the new canonical state",
            ),
            credentials: creds,
        },
    )
    .await
    .expect("the resynced proposal is accepted");
    assert_eq!(
        accepted.delta.prev_commitment, setup.expected_queue_commitments[0],
        "pinned to the new canonical state"
    );
}

#[tokio::test]
async fn depth_one_is_the_single_candidate_gate() {
    // The default depth: queueing is an opt-in. The signer-changing chain
    // is fine here: with one candidate in flight nothing is judged on
    // what it changes, and after it promotes the queue is empty.
    let mut setup = queue_setup(1).await;
    assert_eq!(
        crate::canonicalization::CanonicalizationConfig::default()
            .max_pending_candidates_per_account,
        1
    );
    setup
        .push(setup.fixture_delta(1))
        .await
        .expect("delta 1 is admitted on the canonical base");

    // With a candidate in flight every delta waits, whatever its base:
    // chained from the candidate, competing for the canonical base, or on
    // a state the server does not know (409 before the queue existed, so
    // never a 400 commitment mismatch here).
    let mut unknown_base = setup.fixture_delta(2);
    unknown_base.prev_commitment = format!("0x{}", "ee".repeat(32));
    let mut competing = setup.fixture_delta(1);
    competing.nonce = 2;
    for (label, delta) in [
        ("chained", setup.fixture_delta(2)),
        ("competing", competing),
        ("unknown base", unknown_base),
    ] {
        let refused = setup
            .push(delta)
            .await
            .expect_err("a second delta waits for the first");
        assert!(
            matches!(refused, GuardianError::ConflictPendingDelta),
            "{label}: expected ConflictPendingDelta, got {refused:?}"
        );
    }
    let creds = setup.credentials();
    let refused = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 2,
            delta_payload: proposal_payload(&setup.fixture_delta(2), "waits"),
            credentials: creds,
        },
    )
    .await
    .expect_err("a proposal waits for the candidate too");
    assert!(matches!(refused, GuardianError::ConflictPendingDelta));

    // Once the candidate promotes, the next delta goes through.
    setup
        .chain
        .set_on_chain_commitment(&setup.account_id, &setup.expected_commitments[0]);
    process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    let second = setup
        .push(setup.fixture_delta(2))
        .await
        .expect("the next delta is admitted after promotion");
    assert_eq!(
        second.new_commitment.as_deref(),
        Some(setup.expected_commitments[1].as_str())
    );
}

#[tokio::test]
async fn successor_behind_a_signer_changing_candidate_is_refused_until_it_promotes() {
    let mut setup = queue_setup(4).await;
    let keys: serde_json::Value = serde_json::from_str(fixtures::KEYS_JSON).expect("keys.json");
    let added_signer = keys["signer_4_commitment"]
        .as_str()
        .expect("signer 4 commitment")
        .to_string();

    // `delta_1` adds a fourth signer. It is admitted like any candidate...
    let head = setup
        .push(setup.fixture_delta(1))
        .await
        .expect("a signer-changing delta is admitted on the canonical base");
    assert!(head.status.is_candidate());

    // ...but while it is queued, requests are still authorized against the
    // canonical signer set, so nothing chains behind it: not the delta
    // built on its post-state, not a proposal at the next nonce, although
    // the queue has room for three more.
    let refused = setup
        .push(setup.fixture_delta(2))
        .await
        .expect_err("a delta behind a signer-changing candidate is refused");
    assert!(
        matches!(refused, GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );
    let creds = setup.credentials();
    let refused = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 2,
            delta_payload: proposal_payload(&setup.fixture_delta(2), "behind a signer change"),
            credentials: creds,
        },
    )
    .await
    .expect_err("a proposal behind a signer-changing candidate is refused");
    assert!(
        matches!(refused, GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );
    assert_eq!(
        setup
            .state
            .storage
            .pull_candidate_deltas(&setup.account_id)
            .await
            .expect("queue readable")
            .len(),
        1,
        "only the signer-changing candidate is queued"
    );
    assert!(
        setup
            .state
            .storage
            .pull_pending_proposals(&setup.account_id)
            .await
            .expect("proposals readable")
            .is_empty(),
        "nothing is stored for cosigners to sign"
    );
    let auth_before = setup
        .state
        .metadata
        .get(&setup.account_id)
        .await
        .expect("metadata readable")
        .expect("metadata present")
        .auth;
    assert_eq!(auth_before.cosigner_commitments().len(), 3);

    // The change lands and promotes: the cosigner set is synced from the
    // new canonical state, and the successor is admitted on it.
    setup
        .chain
        .set_on_chain_commitment(&setup.account_id, &setup.expected_commitments[0]);
    let pass = process_canonicalizations_now(&setup.state)
        .await
        .expect("worker pass succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(setup.delta(1).await.status.is_canonical());
    let auth_after = setup
        .state
        .metadata
        .get(&setup.account_id)
        .await
        .expect("metadata readable")
        .expect("metadata present")
        .auth;
    assert_eq!(auth_after.cosigner_commitments().len(), 4);
    assert!(auth_after.cosigner_commitments().contains(&added_signer));
    let successor = setup
        .push(setup.fixture_delta(2))
        .await
        .expect("the successor is admitted once the signer change is canonical");
    assert_eq!(
        successor.new_commitment.as_deref(),
        Some(setup.expected_commitments[1].as_str())
    );
}

#[tokio::test]
async fn proposal_must_extend_the_queue_by_exactly_one_nonce() {
    let mut setup = queue_setup(4).await;
    let queued = setup
        .push(setup.queue_delta(1))
        .await
        .expect("the head is queued");
    assert_eq!(queued.nonce, 1);

    // At the tail's nonce (built on the canonical state), past the tail's
    // nonce plus one (a gap), and a timestamp label: none was derived from
    // the tail, none is recorded.
    for nonce in [1, 3, 1_790_941_600_874] {
        let creds = setup.credentials();
        let refused = push_delta_proposal(
            &setup.state,
            PushDeltaProposalParams {
                account_id: setup.account_id.clone(),
                nonce,
                delta_payload: proposal_payload(&setup.queue_delta(2), "does not extend the queue"),
                credentials: creds,
            },
        )
        .await
        .expect_err("a proposal that does not extend the queue by one is refused");
        assert!(
            matches!(refused, GuardianError::ConflictPendingDelta),
            "nonce {nonce}: expected ConflictPendingDelta, got {refused:?}"
        );
    }
    assert!(
        setup
            .state
            .storage
            .pull_pending_proposals(&setup.account_id)
            .await
            .expect("proposals readable")
            .is_empty(),
        "nothing is stored for cosigners to sign"
    );

    // The tail's nonce plus one is recorded, pinned to the tail.
    let creds = setup.credentials();
    let accepted = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 2,
            delta_payload: proposal_payload(&setup.queue_delta(2), "extends the queue"),
            credentials: creds,
        },
    )
    .await
    .expect("the tail's nonce plus one is recorded");
    assert_eq!(accepted.delta.nonce, 2);
    assert_eq!(
        accepted.delta.prev_commitment, setup.expected_queue_commitments[0],
        "pinned to the tail"
    );
}
