//! End-to-end confirmation of the issue #434 mechanism: the release sweep
//! releases an account whose guardian switch never reached the push path.
//!
//! `switch_guardian_canonicalization.rs` covers the push path — the
//! wallet pushes the `SwitchGuardian` delta to the old guardian, it
//! canonicalizes, the hook releases. This test reproduces the blind spot
//! that path cannot cover: the switch executes on chain and **no delta is
//! ever pushed** (offline switch path, failed best-effort push, client
//! predating the push, a network-dead old operator). The old guardian's
//! stored state stays pre-switch, and only the chain can tell it that the
//! account left.
//!
//! 1. Build a real 2-of-2 multisig account whose guardian key is THIS
//!    server's ack key, exactly like `/configure` enforces.
//! 2. Execute the signed `update_guardian_public_key` transaction on a
//!    mock chain for the authoritative post-switch state.
//! 3. Onboard the pre-switch state on this (soon to be old) guardian and
//!    push nothing. Register the executed commitment as the on-chain
//!    answer and the executed state as the account's published storage.
//! 4. Run the sweep's process-now entry point. Assert it released the
//!    account with the chain-sweep audit evidence, that a still-bound
//!    account is left alone, and that a private account is released only
//!    through a pending switch proposal whose post-state the chain
//!    reached (the proposal-match detector), never on an opaque read.

use std::sync::Arc;

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use guardian_shared::{SignatureScheme, ToJson};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::account::{Account, AccountType};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::AuthGuardedMultisig;
use miden_standards::code_builder::CodeBuilder;
use miden_testing::MockChainBuilder;
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};

use crate::delta_object::DeltaObject;
use crate::jobs::release_sweep::run_release_sweep_now;
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::services::{
    ConfigureAccountParams, PushDeltaParams, PushDeltaProposalParams, configure_account,
    push_delta, push_delta_proposal,
};
use crate::state::AppState;
use crate::testing::helpers::{
    CapturingAuditor, IntegrationMockNetworkClient, create_test_app_state,
};

fn commitment_hex(account: &Account) -> String {
    format!("0x{}", hex::encode(account.to_commitment().as_bytes()))
}

fn word_from_hex(hex_str: &str) -> Word {
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

/// A guardian-bound account onboarded on this server with its pre-switch
/// state, plus the authoritative post-switch state the chain holds after
/// a switch this server never heard about.
struct UnannouncedSwitch {
    state: AppState,
    /// The abort `TransactionSummary` the wallet pushes as the switch
    /// proposal / delta payload.
    switch_summary: serde_json::Value,
    auditor: CapturingAuditor,
    account_id_hex: String,
    pre_switch_account: Account,
    pre_switch_commitment: String,
    executed_account: Account,
    executed_commitment: String,
    new_guardian_commitment_hex: String,
    ack_commitment_hex: String,
    cosigner_key: SecretKey,
    api_pubkey_hex: String,
    all_commitments_hex: Vec<String>,
    next_timestamp: i64,
}

impl UnannouncedSwitch {
    fn credentials(&mut self) -> Credentials {
        self.next_timestamp += 1;
        falcon_credentials(
            &self.cosigner_key,
            &self.api_pubkey_hex,
            &self.account_id_hex,
            self.next_timestamp,
        )
    }

    /// Install the chain view: `on_chain` is the account state the chain
    /// holds, so the node reports its commitment and — when `publish` is
    /// set — serves its storage. `publish = false` models a private
    /// account: the commitment is visible, the storage is not. A real
    /// node always returns storage together with the commitment it
    /// belongs to, so the two are never mixed here.
    fn install_chain(&mut self, on_chain: &Account, publish: bool) {
        let miden_client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
        let mut integration_client = IntegrationMockNetworkClient::new(miden_client);
        integration_client.register_account(self.account_id_hex.clone(), commitment_hex(on_chain));
        if publish {
            integration_client.publish_state(self.account_id_hex.clone(), on_chain.to_json());
        }
        self.state.network_client = Arc::new(integration_client);
    }

    /// The pre-switch account advanced by one transaction that kept this
    /// server's guardian key: the "stored state lags the chain" case.
    fn advanced_still_bound_account(&self) -> Account {
        let mut advanced = self.pre_switch_account.clone();
        advanced
            .increment_nonce(Felt::ONE)
            .expect("nonce increments");
        assert_ne!(commitment_hex(&advanced), self.pre_switch_commitment);
        assert_eq!(
            MidenAccountInspector::new(&advanced)
                .extract_guardian_public_key()
                .as_deref(),
            Some(self.ack_commitment_hex.as_str()),
            "advanced state must still carry this server's guardian key"
        );
        advanced
    }

    async fn released_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.state
            .metadata
            .get(&self.account_id_hex)
            .await
            .expect("metadata readable")
            .expect("metadata present")
            .released_at
    }
}

async fn unannounced_switch() -> UnannouncedSwitch {
    unannounced_switch_for(AccountType::Public).await
}

async fn unannounced_switch_for(account_type: AccountType) -> UnannouncedSwitch {
    let mut state = create_test_app_state().await;

    // The account's guardian key is this server's ack key, mirroring the
    // binding `/configure` enforces in production.
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
        .with_account_type(account_type)
        .with_signature_scheme(SignatureScheme::Falcon);
    let multisig_account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");
    assert_eq!(
        multisig_account.id().is_public(),
        account_type == AccountType::Public,
        "fixture visibility must follow the requested account type"
    );

    let account_id_hex = multisig_account.id().to_hex();
    let pre_switch_commitment = commitment_hex(&multisig_account);

    let mock_chain = MockChainBuilder::with_accounts([multisig_account.clone()])
        .expect("mock chain accepts account")
        .build()
        .expect("mock chain builds");

    // The switch target: a fresh guardian key unrelated to this server.
    let new_guardian_key = SecretKey::new();
    let new_guardian_commitment = new_guardian_key.public_key().to_commitment();
    let new_guardian_commitment_hex =
        format!("0x{}", hex::encode(new_guardian_commitment.to_bytes()));

    let new_guardian_scheme_id = SignatureScheme::Falcon.auth_scheme_id();
    let tx_script_code = format!(
        "@transaction_script\npub proc main\n    push.{new_guardian_commitment}\n    push.{new_guardian_scheme_id}\n    call.::miden::standards::components::auth::guarded_multisig::update_guardian_public_key\n    drop\n    dropw\nend"
    );
    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_package(AuthGuardedMultisig::code())
        .expect("library links")
        .compile_tx_script(&tx_script_code)
        .expect("tx script compiles");
    // Bare salt: `MockChain` charges no fee, so no fee note is created
    // (see switch_guardian_canonicalization.rs for the caveat).
    let salt = Word::from([Felt::new_unchecked(7); 4]);

    // The mock chain holds no state for private accounts, so the
    // transaction is built from the account itself for both visibilities.
    let abort_summary = match mock_chain
        .build_transaction(multisig_account.clone())
        .authenticator(None)
        .tx_script(tx_script.clone())
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
    let msg = abort_summary.as_ref().to_commitment();
    let switch_summary = abort_summary.as_ref().to_json();
    let signing = SigningInputs::TransactionSummary(abort_summary);
    let authenticator_1 =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(cosigner_keys[0].clone())]);
    let authenticator_2 =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(cosigner_keys[1].clone())]);
    let sig_1 = authenticator_1
        .get_signature(cosigner_pubkeys[0].to_commitment().into(), &signing)
        .await
        .expect("cosigner 1 signs");
    let sig_2 = authenticator_2
        .get_signature(cosigner_pubkeys[1].to_commitment().into(), &signing)
        .await
        .expect("cosigner 2 signs");

    // The authoritative on-chain result: the switch executed with the
    // multisig threshold alone — the one transaction that needs no
    // guardian signature, which is exactly why this server never saw it.
    let executed_tx = mock_chain
        .build_transaction(multisig_account.clone())
        .authenticator(None)
        .tx_script(tx_script)
        .add_signature(cosigner_pubkeys[0].clone().into(), msg, sig_1)
        .add_signature(cosigner_pubkeys[1].clone().into(), msg, sig_2)
        .auth_args(salt)
        .build()
        .expect("tx builds")
        .execute()
        .await
        .expect("signed switch executes");
    let mut executed_account = multisig_account.clone();
    executed_account
        .apply_patch(executed_tx.account_patch())
        .expect("executed patch applies");
    let executed_commitment = commitment_hex(&executed_account);
    assert_ne!(executed_commitment, pre_switch_commitment);
    assert_eq!(
        MidenAccountInspector::new(&executed_account)
            .extract_guardian_public_key()
            .as_deref(),
        Some(new_guardian_commitment_hex.as_str()),
        "post-switch state must carry the NEW guardian's key"
    );

    let auditor = CapturingAuditor::new();
    state.auditor = Arc::new(auditor.clone());

    let api_pubkey_hex = cosigner_pubkeys[0].clone().into_hex();
    let all_commitments_hex: Vec<String> = cosigner_pubkeys
        .iter()
        .map(|pk| format!("0x{}", hex::encode(pk.to_commitment().to_bytes())))
        .collect();

    let mut fixture = UnannouncedSwitch {
        state,
        switch_summary,
        auditor,
        account_id_hex: account_id_hex.clone(),
        pre_switch_account: multisig_account.clone(),
        pre_switch_commitment,
        executed_account,
        executed_commitment,
        new_guardian_commitment_hex,
        ack_commitment_hex,
        cosigner_key: cosigner_keys[0].clone(),
        api_pubkey_hex,
        all_commitments_hex,
        next_timestamp: chrono::Utc::now().timestamp_millis(),
    };

    // Onboard the PRE-switch state while the chain is still at it (the
    // integration client learns the account at configure time), so the
    // chain view can be re-pointed afterwards without a candidate.
    let miden_client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
    fixture.state.network_client = Arc::new(IntegrationMockNetworkClient::new(miden_client));
    let creds = fixture.credentials();
    configure_account(
        &fixture.state,
        ConfigureAccountParams {
            account_id: account_id_hex,
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: fixture.all_commitments_hex.clone(),
            },
            network_config: NetworkConfig::miden_default(),
            initial_state: multisig_account.to_json(),
            credential: creds,
        },
    )
    .await
    .expect("configure_account succeeds");
    assert!(fixture.released_at().await.is_none());

    fixture
}

#[tokio::test]
async fn test_release_sweep_releases_account_whose_switch_never_reached_the_push_path() {
    let mut fixture = unannounced_switch().await;
    // The chain moved to the post-switch state and publishes it; this
    // server still holds the pre-switch state and never received a delta.
    let executed = fixture.executed_account.clone();
    fixture.install_chain(&executed, true);

    let stored_before = fixture
        .state
        .storage
        .pull_state(&fixture.account_id_hex)
        .await
        .expect("state readable");
    assert_eq!(stored_before.commitment, fixture.pre_switch_commitment);
    let deltas = fixture
        .state
        .storage
        .pull_deltas_after(&fixture.account_id_hex, 0)
        .await
        .expect("deltas readable");
    assert!(deltas.is_empty(), "no delta was ever pushed for the switch");

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(!pass.cancelled);

    // Released by the sweep, with the chain observation on the audit row.
    let released_at = fixture.released_at().await;
    assert!(
        released_at.is_some(),
        "the release sweep must release an account whose on-chain guardian key differs"
    );
    let release_events: Vec<_> = fixture
        .auditor
        .snapshot()
        .into_iter()
        .filter(|e| e.action_kind == crate::audit::kinds::ACCOUNTS_RELEASE)
        .collect();
    assert_eq!(
        release_events.len(),
        1,
        "exactly one accounts.release audit event"
    );
    let event = &release_events[0];
    assert_eq!(
        event.operator_identity,
        crate::services::release_on_switch::SYSTEM_OPERATOR_IDENTITY
    );
    assert_eq!(
        event.target_account_id.as_deref(),
        Some(fixture.account_id_hex.as_str())
    );
    assert_eq!(event.payload["detected_by"], "chain_sweep");
    assert_eq!(
        event.payload["new_guardian_commitment"],
        fixture.new_guardian_commitment_hex
    );
    assert_eq!(
        event.payload["on_chain_commitment"],
        fixture.executed_commitment
    );
    assert_eq!(
        event.payload["stored_commitment"],
        fixture.pre_switch_commitment
    );
    assert_ne!(
        event.payload["new_guardian_commitment"],
        fixture.ack_commitment_hex
    );

    // The stored state is untouched (reads keep serving the last state
    // this server verified) and a second pass neither re-releases nor
    // re-audits.
    let stored_after = fixture
        .state
        .storage
        .pull_state(&fixture.account_id_hex)
        .await
        .expect("released account state must remain readable");
    assert_eq!(stored_after.commitment, fixture.pre_switch_commitment);
    run_release_sweep_now(&fixture.state)
        .await
        .expect("second sweep succeeds");
    assert_eq!(fixture.released_at().await, released_at);
    assert_eq!(
        fixture
            .auditor
            .snapshot()
            .iter()
            .filter(|e| e.action_kind == crate::audit::kinds::ACCOUNTS_RELEASE)
            .count(),
        1
    );

    // Mutations are refused with the release-specific error...
    let creds = fixture.credentials();
    let rejected = push_delta(
        &fixture.state,
        PushDeltaParams {
            delta: DeltaObject {
                account_id: fixture.account_id_hex.clone(),
                nonce: fixture.pre_switch_account.nonce().as_canonical_u64() + 1,
                prev_commitment: fixture.pre_switch_commitment.clone(),
                new_commitment: None,
                delta_payload: serde_json::json!({}),
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
    .expect_err("released account must refuse mutations");
    assert!(
        matches!(
            rejected,
            crate::error::GuardianError::AccountReleased { .. }
        ),
        "expected AccountReleased, got {rejected:?}"
    );

    // ...and re-onboarding via /configure (the switch-back case) reactivates.
    let creds = fixture.credentials();
    let executed_state = fixture.executed_account.to_json();
    configure_account(
        &fixture.state,
        ConfigureAccountParams {
            account_id: fixture.account_id_hex.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: fixture.all_commitments_hex.clone(),
            },
            network_config: NetworkConfig::miden_default(),
            initial_state: executed_state,
            credential: creds,
        },
    )
    .await
    .expect("re-onboarding a released account succeeds");
    assert!(fixture.released_at().await.is_none());
}

#[tokio::test]
async fn test_release_sweep_leaves_a_still_bound_lagging_account_alone() {
    // The chain moved past the stored base (a transaction this server
    // acknowledged but never promoted), and the storage it publishes for
    // that advanced state still carries THIS server's guardian key: a
    // stored-state lag, not a switch. The sweep must not release.
    let mut fixture = unannounced_switch().await;
    let advanced = fixture.advanced_still_bound_account();
    fixture.install_chain(&advanced, true);

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(
        fixture.released_at().await.is_none(),
        "an account still bound to this guardian must never be released by the sweep"
    );
    assert!(
        fixture
            .auditor
            .snapshot()
            .iter()
            .all(|e| e.action_kind != crate::audit::kinds::ACCOUNTS_RELEASE)
    );
}

#[tokio::test]
async fn test_release_sweep_cannot_verify_a_private_account_without_a_pending_proposal() {
    // A private account switched without leaving a proposal on this
    // server (offline switch): the chain publishes no storage, no
    // pending proposal explains the new commitment, and the sweep
    // records that it cannot tell rather than guessing.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    let executed = fixture.executed_account.clone();
    fixture.install_chain(&executed, false);

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(
        fixture.released_at().await.is_none(),
        "without published storage or a matching proposal the sweep has no evidence"
    );
    assert!(
        fixture
            .auditor
            .snapshot()
            .iter()
            .all(|e| e.action_kind != crate::audit::kinds::ACCOUNTS_RELEASE)
    );
}

#[tokio::test]
async fn test_release_sweep_matches_a_pending_switch_proposal_for_a_private_account() {
    // The normal online flow: the wallet creates the switch proposal on
    // this (old) guardian, then executes the switch elsewhere. The
    // proposal sits pending here; applying its summary to the stored
    // state gives exactly the commitment the chain now holds, which
    // proves the switch executed — with no published storage at all.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    let executed_nonce = fixture.executed_account.nonce().as_canonical_u64();
    let creds = fixture.credentials();
    let proposal = push_delta_proposal(
        &fixture.state,
        PushDeltaProposalParams {
            account_id: fixture.account_id_hex.clone(),
            nonce: executed_nonce,
            delta_payload: serde_json::json!({
                "tx_summary": fixture.switch_summary.clone(),
                "signatures": [],
                "metadata": {
                    "proposal_type": "switch_guardian",
                    "description": "switch to the new operator",
                    "new_guardian_endpoint": "https://new-guardian.example",
                    "new_guardian_pubkey": fixture.new_guardian_commitment_hex.clone(),
                    "required_signatures": 2
                }
            }),
            credentials: creds,
        },
    )
    .await
    .expect("the switch proposal is accepted on the old guardian");
    let proposal_id = proposal.commitment.clone();

    // The switch executes on chain; this server never receives a delta.
    let executed = fixture.executed_account.clone();
    fixture.install_chain(&executed, false);

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(
        fixture.released_at().await.is_some(),
        "the proposal match must release a private account"
    );
    let release_events: Vec<_> = fixture
        .auditor
        .snapshot()
        .into_iter()
        .filter(|e| e.action_kind == crate::audit::kinds::ACCOUNTS_RELEASE)
        .collect();
    assert_eq!(release_events.len(), 1);
    let event = &release_events[0];
    assert_eq!(event.payload["detected_by"], "proposal_match");
    assert_eq!(event.payload["proposal_id"], proposal_id);
    assert_eq!(
        event.payload["new_guardian_commitment"],
        fixture.new_guardian_commitment_hex
    );
    assert_eq!(
        event.payload["on_chain_commitment"],
        fixture.executed_commitment
    );
    assert_eq!(
        event.payload["stored_commitment"],
        fixture.pre_switch_commitment
    );

    // The executed proposal no longer lingers as pending.
    let remaining = fixture
        .state
        .storage
        .pull_pending_proposals(&fixture.account_id_hex)
        .await
        .expect("proposals readable");
    assert!(
        remaining.is_empty(),
        "the switch proposal the chain proved executed is finalized"
    );
}
