//! End-to-end coverage of the per-account candidate queue (issue #17)
//! over the real Miden delta-application path, driven by the fixture
//! chain `account.json` → `delta_1` → `delta_2` → `delta_3` (whose
//! post-state commitments are pinned in `commitments.json`).
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
    next_timestamp: i64,
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

    fn fixture_delta(&self, delta_num: u8) -> DeltaObject {
        let fixture = crate::testing::helpers::load_fixture_delta(delta_num);
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

    async fn push(&mut self, delta: DeltaObject) -> Result<DeltaObject, GuardianError> {
        let credentials = self.credentials();
        push_delta(&self.state, PushDeltaParams { delta, credentials })
            .await
            .map(|result| result.delta)
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
    let expected_commitments = (1..=3)
        .map(|i| {
            commitments[format!("commitment_after_delta_{i}")]
                .as_str()
                .expect("pinned commitment")
                .to_string()
        })
        .collect();

    let keys: serde_json::Value = serde_json::from_str(fixtures::KEYS_JSON).expect("keys.json");
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
        .push(setup.fixture_delta(1))
        .await
        .expect("delta 1 is admitted on the canonical base");
    assert!(first.status.is_candidate());
    assert_eq!(
        first.new_commitment.as_deref(),
        Some(setup.expected_commitments[0].as_str())
    );
    let second = setup
        .push(setup.fixture_delta(2))
        .await
        .expect("delta 2 is admitted on the queue tail without waiting");
    assert!(second.status.is_candidate());
    assert_eq!(
        second.new_commitment.as_deref(),
        Some(setup.expected_commitments[1].as_str())
    );
    assert_eq!(
        setup.stored_commitment().await,
        setup.initial_commitment,
        "the canonical state does not move on admission"
    );

    // Competing for the canonical base while delta 1 holds it.
    let mut competing = setup.fixture_delta(1);
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
        .push(setup.fixture_delta(3))
        .await
        .expect_err("queue depth is enforced");
    assert!(
        matches!(refused, GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );
    // ...and a proposal is refused up front for the same reason.
    let proposal_payload = serde_json::json!({
        "tx_summary": setup.fixture_delta(3).delta_payload,
        "signatures": [],
        "metadata": {
            "proposal_type": "custom",
            "description": "proposed against the queue tail"
        }
    });
    let creds = setup.credentials();
    let refused = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id.clone(),
            nonce: 3,
            delta_payload: proposal_payload.clone(),
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
        .push(setup.fixture_delta(3))
        .await
        .expect("delta 3 is admitted once the depth allows");
    assert_eq!(
        third.new_commitment.as_deref(),
        Some(setup.expected_commitments[2].as_str())
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
            delta_payload: proposal_payload,
            credentials: creds,
        },
    )
    .await
    .expect("a proposal is accepted against the queue tail");
    assert_eq!(
        proposal.delta.prev_commitment, setup.expected_commitments[2],
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
        .set_on_chain_commitment(&setup.account_id, &setup.expected_commitments[2]);
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
            Some(setup.expected_commitments[index].as_str())
        );
    }
    assert_eq!(
        setup.stored_commitment().await,
        setup.expected_commitments[2],
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
        .push(setup.fixture_delta(1))
        .await
        .expect("delta 1 is admitted");
    setup
        .push(setup.fixture_delta(2))
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
        .push(setup.fixture_delta(1))
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
        .set_on_chain_commitment(&setup.account_id, &setup.expected_commitments[1]);
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
            Some(setup.expected_commitments[index].as_str())
        );
    }
    assert_eq!(
        setup.stored_commitment().await,
        setup.expected_commitments[1]
    );
}
