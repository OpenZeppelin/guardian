//! End-to-end reproduction of issue #319 and confirmation of the fix.
//!
//! The issue's scenario: an approved candidate whose transaction dies
//! client-side after approval (RPC submit failure, prover timeout, crash)
//! holds the account's pending-candidate lock for the full submission
//! grace period plus retry budget, and every new proposal in that window
//! is answered `409 conflict_pending_delta`. This test replays that
//! sequence against a real mock chain:
//!
//! 1. Build a real 2-of-2 multisig account (guardian key = this server's
//!    ack key), obtain a valid abort `TransactionSummary` exactly like the
//!    wallet does, and configure the account on the server.
//! 2. Push the delta so it becomes a candidate, with the registered
//!    on-chain answer pinned at the account's INITIAL commitment — the
//!    transaction never lands, mirroring the client-side death.
//! 3. Run the canonicalization worker and assert the candidate survives
//!    (submission grace period), then assert a new proposal is refused
//!    with `ConflictPendingDelta` — the issue's 409 loop.
//! 4. Record the abandon intent via the client-initiated service (202
//!    semantics: the account stays locked), run the worker to resolve the
//!    quarantine, and assert the delta transitions to
//!    `Discarded { reason: ClientAbandoned }` (kept as history), the flag
//!    clears, and the SAME proposal is accepted afterwards.
//!
//! A second test covers the safety guard: when the registered on-chain
//! answer equals the candidate's expected post-state (the transaction
//! actually landed), abandon must refuse with `CandidateLanded` and leave
//! the candidate for the worker to canonicalize.

use std::sync::Arc;

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use guardian_shared::{SignatureScheme, ToJson};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::{Account, AccountType};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::Serializable;
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::AuthGuardedMultisig;
use miden_standards::code_builder::CodeBuilder;
use miden_testing::MockChainBuilder;
use miden_tx::TransactionExecutorError;

use crate::delta_object::DeltaObject;
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
use crate::testing::helpers::{IntegrationMockNetworkClient, create_test_app_state};

fn commitment_hex(account: &Account) -> String {
    format!("0x{}", hex::encode(account.to_commitment().as_bytes()))
}

fn word_from_hex(hex_str: &str) -> Word {
    use miden_protocol::utils::serde::Deserializable;
    let bytes = hex::decode(hex_str.trim_start_matches("0x")).expect("valid hex");
    Word::read_from_bytes(&bytes).expect("valid word bytes")
}

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

/// Scaffolding shared by both #319 e2e tests: a configured 2-of-2
/// multisig account with a pushed candidate delta whose on-chain answer
/// is pinned to `registered_commitment` (computed from the setup's own
/// artifacts by the caller-provided selector).
struct StrandedCandidateSetup {
    state: AppState,
    account_id_hex: String,
    candidate_nonce: u64,
    cosigner_key: SecretKey,
    api_pubkey_hex: String,
    delta_payload: serde_json::Value,
    /// Commitment of the candidate's expected post-state, as the server
    /// itself computes it via `apply_delta`.
    expected_commitment_hex: String,
    /// Strictly-increasing auth timestamp source (replay protection).
    next_timestamp: i64,
}

impl StrandedCandidateSetup {
    fn credentials(&mut self) -> Credentials {
        self.next_timestamp += 1;
        falcon_credentials(
            &self.cosigner_key,
            &self.api_pubkey_hex,
            &self.account_id_hex,
            self.next_timestamp,
        )
    }

    /// The wallet-shaped proposal payload the issue's 409 loop keeps
    /// retrying: the same valid `TransactionSummary`, wrapped as a
    /// multisig proposal.
    fn proposal_payload(&self) -> serde_json::Value {
        serde_json::json!({
            "tx_summary": self.delta_payload.clone(),
            "signatures": [],
            "metadata": {
                "proposal_type": "custom",
                "description": "retried while candidate was stuck"
            }
        })
    }
}

/// `landed`: whether the registered on-chain commitment is the candidate's
/// expected post-state (transaction landed) or the account's initial
/// commitment (transaction never landed — the issue's scenario).
async fn stranded_candidate_setup(landed: bool) -> StrandedCandidateSetup {
    let mut state = create_test_app_state().await;

    let scheme = SignatureScheme::Falcon;
    let ack_commitment_hex = state.ack.commitment(&scheme);
    let ack_commitment_word = word_from_hex(&ack_commitment_hex);

    let cosigner_keys: Vec<SecretKey> = (0..2).map(|_| SecretKey::new()).collect();
    let cosigner_pubkeys: Vec<_> = cosigner_keys.iter().map(|k| k.public_key()).collect();
    let signer_commitments: Vec<Word> = cosigner_pubkeys
        .iter()
        .map(|pk| pk.to_commitment())
        .collect();

    let config = MultisigGuardianConfig::new(2, signer_commitments, ack_commitment_word)
        .with_account_type(AccountType::Public)
        .with_signature_scheme(SignatureScheme::Falcon);
    let multisig_account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");

    let account_id_hex = multisig_account.id().to_hex();
    let initial_commitment = commitment_hex(&multisig_account);

    let mock_chain = MockChainBuilder::with_accounts([multisig_account.clone()])
        .expect("mock chain accepts account")
        .build()
        .expect("mock chain builds");

    // Any valid transaction works; the guardian-key update script is the
    // one with existing scaffolding. The no-signature abort run yields the
    // TransactionSummary the wallet pushes as the delta payload.
    let new_guardian_key = SecretKey::new();
    let new_guardian_commitment = new_guardian_key.public_key().to_commitment();
    let new_guardian_scheme_id = SignatureScheme::Falcon.auth_scheme_id();
    let tx_script_code = format!(
        "@transaction_script\npub proc main\n    push.{new_guardian_commitment}\n    push.{new_guardian_scheme_id}\n    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key\n    drop\n    dropw\nend"
    );
    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(AuthGuardedMultisig::code())
        .expect("library links")
        .compile_tx_script(&tx_script_code)
        .expect("tx script compiles");
    let salt = Word::from([Felt::new_unchecked(7); 4]);

    let abort_summary = match mock_chain
        .build_tx_context(multisig_account.id(), &[], &[])
        .expect("tx context builds")
        .authenticator(None)
        .tx_script(tx_script)
        .auth_args(salt)
        .build()
        .expect("tx builds")
        .execute()
        .await
        .unwrap_err()
    {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };
    let delta_payload = abort_summary.as_ref().to_json();

    // The candidate's expected post-state commitment, computed the same
    // way the server computes it (apply_delta on the initial state).
    let miden_client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
    let (_, expected_commitment_hex) = {
        use crate::network::NetworkClient;
        miden_client
            .apply_delta(&multisig_account.to_json(), &delta_payload)
            .expect("delta applies to initial state")
    };

    let registered_commitment = if landed {
        expected_commitment_hex.clone()
    } else {
        initial_commitment.clone()
    };
    let mut integration_client = IntegrationMockNetworkClient::new(miden_client);
    integration_client.register_account(account_id_hex.clone(), registered_commitment);
    state.network_client = Arc::new(integration_client);

    let api_pubkey_hex = cosigner_pubkeys[0].clone().into_hex();
    // /configure validates the declared cosigner set against the state's
    // signer map (#102), so the full set is registered.
    let all_commitments_hex: Vec<String> = cosigner_pubkeys
        .iter()
        .map(|pk| format!("0x{}", hex::encode(pk.to_commitment().to_bytes())))
        .collect();

    let mut setup = StrandedCandidateSetup {
        state,
        account_id_hex: account_id_hex.clone(),
        candidate_nonce: multisig_account.nonce().as_canonical_u64() + 1,
        cosigner_key: cosigner_keys[0].clone(),
        api_pubkey_hex,
        delta_payload: delta_payload.clone(),
        expected_commitment_hex,
        next_timestamp: chrono::Utc::now().timestamp_millis(),
    };

    let creds = setup.credentials();
    configure_account(
        &setup.state,
        ConfigureAccountParams {
            account_id: account_id_hex.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: all_commitments_hex,
            },
            network_config: NetworkConfig::miden_default(),
            initial_state: multisig_account.to_json(),
            credential: creds,
        },
    )
    .await
    .expect("configure_account succeeds");

    // Push the approved delta: it becomes the pending candidate whose
    // transaction (in the `landed = false` case) will never land.
    let creds = setup.credentials();
    let push_result = push_delta(
        &setup.state,
        PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id_hex.clone(),
                nonce: setup.candidate_nonce,
                prev_commitment: initial_commitment,
                new_commitment: None,
                delta_payload,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: Default::default(),
                metadata: None,
            },
            credentials: creds,
        },
    )
    .await
    .expect("push_delta accepts the delta");
    assert!(
        push_result.delta.status.is_candidate(),
        "delta should await canonicalization, got {:?}",
        push_result.delta.status
    );

    setup
}

#[tokio::test]
async fn test_stranded_candidate_locks_account_until_abandoned() {
    let mut setup = stranded_candidate_setup(false).await;

    // The worker cannot release the candidate: on-chain still shows the
    // account's base state, indistinguishable from slow proving, so the
    // candidate sits in the submission grace period.
    process_canonicalizations_now(&setup.state)
        .await
        .expect("canonicalization run succeeds");
    let deltas = setup
        .state
        .storage
        .pull_deltas_after(&setup.account_id_hex, 0)
        .await
        .expect("deltas readable");
    assert!(
        deltas
            .iter()
            .any(|d| d.nonce == setup.candidate_nonce && d.status.is_candidate()),
        "candidate must survive the worker run within the grace period"
    );

    // The issue's 409 loop: every new proposal is refused while the
    // stranded candidate holds the account.
    let proposal_payload = setup.proposal_payload();
    let creds = setup.credentials();
    let refused = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id_hex.clone(),
            nonce: setup.candidate_nonce + 1,
            delta_payload: proposal_payload.clone(),
            credentials: creds,
        },
    )
    .await
    .expect_err("proposal must be refused while the candidate is pending");
    assert!(
        matches!(refused, GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );

    // The fix: the client knows its transaction died and records the
    // abandon intent. The account stays locked until the worker resolves
    // the quarantine.
    let creds = setup.credentials();
    let abandoned = abandon_candidate(
        &setup.state,
        AbandonCandidateParams {
            account_id: setup.account_id_hex.clone(),
            nonce: setup.candidate_nonce,
            credentials: creds,
        },
    )
    .await
    .expect("abandon_candidate accepts the intent for a never-landing candidate");
    assert_eq!(abandoned.nonce, setup.candidate_nonce);
    assert_eq!(abandoned.state, AbandonState::Pending);
    assert!(abandoned.abandon_requested_at.is_some());

    // Still locked: intent alone releases nothing.
    let creds = setup.credentials();
    let still_refused = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id_hex.clone(),
            nonce: setup.candidate_nonce + 1,
            delta_payload: proposal_payload.clone(),
            credentials: creds,
        },
    )
    .await
    .expect_err("account must stay locked until the worker resolves the intent");
    assert!(matches!(still_refused, GuardianError::ConflictPendingDelta));

    // The worker resolves the intent (the e2e worker runs with a zero
    // quarantine): the delta becomes Discarded { ClientAbandoned } —
    // preserved as history — and the account is released.
    process_canonicalizations_now(&setup.state)
        .await
        .expect("canonicalization run succeeds");

    let deltas = setup
        .state
        .storage
        .pull_deltas_after(&setup.account_id_hex, 0)
        .await
        .expect("deltas readable");
    let resolved = deltas
        .iter()
        .find(|d| d.nonce == setup.candidate_nonce)
        .expect("abandoned delta must be preserved as history");
    assert!(
        resolved.status.is_client_abandoned(),
        "delta must be discarded as client-abandoned, got {:?}",
        resolved.status
    );
    let metadata = setup
        .state
        .metadata
        .get(&setup.account_id_hex)
        .await
        .expect("metadata readable")
        .expect("metadata present");
    assert!(
        !metadata.has_pending_candidate,
        "pending-candidate flag must be cleared by the resolution"
    );

    // The SAME proposal that was refused moments ago is now accepted —
    // no grace-period or retry-budget wait.
    let creds = setup.credentials();
    let accepted = push_delta_proposal(
        &setup.state,
        PushDeltaProposalParams {
            account_id: setup.account_id_hex.clone(),
            nonce: setup.candidate_nonce + 1,
            delta_payload: proposal_payload,
            credentials: creds,
        },
    )
    .await
    .expect("proposal must be accepted immediately after the abandon");
    assert!(accepted.delta.status.is_pending());
}

#[tokio::test]
async fn test_abandon_refused_when_candidate_transaction_landed() {
    let mut setup = stranded_candidate_setup(true).await;

    // On-chain already shows the candidate's expected post-state: the
    // transaction landed, so the client's abandon must be refused — the
    // worker will canonicalize shortly.
    let creds = setup.credentials();
    let refused = abandon_candidate(
        &setup.state,
        AbandonCandidateParams {
            account_id: setup.account_id_hex.clone(),
            nonce: setup.candidate_nonce,
            credentials: creds,
        },
    )
    .await
    .expect_err("abandon must be refused when the transaction landed");
    assert!(
        matches!(refused, GuardianError::CandidateLanded { .. }),
        "expected CandidateLanded, got {refused:?}"
    );

    // The candidate is untouched and the worker canonicalizes it against
    // the landed on-chain state.
    process_canonicalizations_now(&setup.state)
        .await
        .expect("canonicalization run succeeds");
    let deltas = setup
        .state
        .storage
        .pull_deltas_after(&setup.account_id_hex, 0)
        .await
        .expect("deltas readable");
    let candidate = deltas
        .iter()
        .find(|d| d.nonce == setup.candidate_nonce)
        .expect("delta still stored");
    assert!(
        candidate.status.is_canonical(),
        "landed candidate must canonicalize, got {:?}",
        candidate.status
    );
    let final_state = setup
        .state
        .storage
        .pull_state(&setup.account_id_hex)
        .await
        .expect("state readable");
    assert_eq!(final_state.commitment, setup.expected_commitment_hex);
}
