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
//!    account is left alone, that a storage read older than the stored
//!    state is ignored, and that a private account is released only
//!    through a pending switch proposal whose post-state the chain
//!    reached — at the head or in its transaction history, even after the
//!    account moved on, and even when the candidate queue recorded it
//!    against a stuck candidate's post-state — never on an opaque read. A
//!    switch built on the stored state while a stuck candidate holds its
//!    nonce is refused up front instead, and releases once the stuck
//!    candidate is parked and the proposal is made again.

use std::sync::Arc;

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use guardian_shared::{SignatureScheme, ToJson};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::account::{
    Account, AccountCodePatch, AccountDelta, AccountStoragePatch, AccountType, AccountVaultDelta,
};
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::transaction::{
    InputNotes, RawOutputNotes, TransactionHeader, TransactionSummary, TransactionSummaryUserParams,
};
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::{AuthGuardedMultisig, MultisigAuthArgs};
use miden_standards::code_builder::CodeBuilder;
use miden_testing::MockChainBuilder;
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};

use super::MultisigAuthArgsExt;
use crate::canonicalization::CanonicalizationConfig;
use crate::delta_object::{DeltaObject, DeltaStatus, RetainReason};
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

/// The summary of a transaction that only bumps the nonce: an ordinary
/// (non-switch) transaction whose delta applies to `account`.
fn nonce_bump_summary(account: &Account) -> serde_json::Value {
    let delta = AccountDelta::new(
        account.id(),
        AccountStoragePatch::default(),
        AccountVaultDelta::default(),
        AccountCodePatch::default(),
        Felt::ONE,
    )
    .expect("nonce-only delta");
    TransactionSummary::new(
        delta,
        InputNotes::new(Vec::new()).expect("no input notes"),
        RawOutputNotes::new(Vec::new()).expect("no output notes"),
        BlockNumber::from(0),
        Word::from([Felt::new_unchecked(9); 4]),
        0,
        TransactionSummaryUserParams::new([Felt::ZERO; 6]),
    )
    .to_json()
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
    /// The switch transaction's header commitments, as the node records
    /// them for `SyncTransactions`.
    switch_tx_initial: String,
    switch_tx_final: String,
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
    /// belongs to, so the two are never mixed here. The node knows no
    /// transactions.
    fn install_chain(&mut self, on_chain: &Account, publish: bool) {
        self.install_chain_with_history(on_chain, publish, &[]);
    }

    /// [`Self::install_chain`] plus the `(block, initial, final)`
    /// transactions the node lists for the account.
    fn install_chain_with_history(
        &mut self,
        on_chain: &Account,
        publish: bool,
        transactions: &[(u32, String, String)],
    ) {
        let miden_client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
        let mut integration_client = IntegrationMockNetworkClient::new(miden_client);
        integration_client.register_account(self.account_id_hex.clone(), commitment_hex(on_chain));
        if publish {
            integration_client.publish_state(self.account_id_hex.clone(), on_chain.to_json());
        }
        for (block, initial, final_commitment) in transactions {
            integration_client.register_transaction(
                self.account_id_hex.clone(),
                *block,
                initial.clone(),
                final_commitment.clone(),
            );
        }
        self.state.network_client = Arc::new(integration_client);
    }

    /// The executed post-switch state advanced by one more transaction
    /// under the new guardian: the chain has moved past the switch.
    fn moved_past_switch_account(&self) -> Account {
        let mut moved = self.executed_account.clone();
        moved.increment_nonce(Felt::ONE).expect("nonce increments");
        assert_ne!(commitment_hex(&moved), self.executed_commitment);
        moved
    }

    /// Push the switch proposal the wallet creates on this (old) guardian
    /// before executing the switch elsewhere; returns its id.
    async fn push_switch_proposal(&mut self) -> String {
        let executed_nonce = self.executed_account.nonce().as_canonical_u64();
        self.try_push_switch_proposal(executed_nonce)
            .await
            .expect("the switch proposal is accepted on the old guardian")
    }

    /// [`Self::push_switch_proposal`] labelled with `nonce`, returning the
    /// server's verdict instead of expecting acceptance.
    async fn try_push_switch_proposal(
        &mut self,
        nonce: u64,
    ) -> Result<String, crate::error::GuardianError> {
        let creds = self.credentials();
        let proposal = push_delta_proposal(
            &self.state,
            PushDeltaProposalParams {
                account_id: self.account_id_hex.clone(),
                nonce,
                delta_payload: serde_json::json!({
                    "tx_summary": self.switch_summary.clone(),
                    "signatures": [],
                    "metadata": {
                        "proposal_type": "switch_guardian",
                        "description": "switch to the new operator",
                        "new_guardian_endpoint": "https://new-guardian.example",
                        "new_guardian_pubkey": self.new_guardian_commitment_hex.clone(),
                        "required_signatures": 2
                    }
                }),
                credentials: creds,
            },
        )
        .await?;
        Ok(proposal.commitment)
    }

    /// Queue a candidate on the stored base through the push path in
    /// candidate mode, with queueing opted in (issue #17): an ordinary
    /// transaction this server acknowledged but that never lands. Returns
    /// its nonce and post-state.
    async fn push_stuck_candidate(&mut self) -> (u64, String) {
        self.state.canonicalization =
            Some(CanonicalizationConfig::default().with_max_pending_candidates_per_account(4));
        let nonce = self.executed_account.nonce().as_canonical_u64();
        let creds = self.credentials();
        let pushed = push_delta(
            &self.state,
            PushDeltaParams {
                delta: DeltaObject {
                    account_id: self.account_id_hex.clone(),
                    nonce,
                    prev_commitment: self.pre_switch_commitment.clone(),
                    delta_payload: nonce_bump_summary(&self.pre_switch_account),
                    ..Default::default()
                },
                credentials: creds,
            },
        )
        .await
        .expect("the candidate is admitted on the stored base");
        assert!(pushed.delta.status.is_candidate());
        (
            nonce,
            pushed
                .delta
                .new_commitment
                .expect("the post-state is recorded"),
        )
    }

    /// Park a candidate the way canonicalization does once the chain has
    /// moved off its base (`retain_candidate`, then the queue-aware flag
    /// release). The process-now test processor never parks on
    /// divergence, so the writes are made directly.
    async fn park_diverged_candidate(&self, nonce: u64) {
        let now = chrono::Utc::now().to_rfc3339();
        self.state
            .storage
            .update_candidate_status(
                &self.account_id_hex,
                nonce,
                DeltaStatus::retained(now.clone(), RetainReason::Diverged),
                None,
            )
            .await
            .expect("the candidate is parked");
        assert!(
            !self
                .state
                .storage
                .has_pending_candidate(&self.account_id_hex)
                .await
                .expect("queue readable"),
            "no candidate is left queued"
        );
        self.state
            .metadata
            .clear_pending_candidate_if_none(&self.account_id_hex, &now)
            .await
            .expect("the pending flag is released");
    }

    fn release_events(&self) -> Vec<crate::audit::AuditEvent> {
        self.auditor
            .snapshot()
            .into_iter()
            .filter(|e| e.action_kind == crate::audit::kinds::ACCOUNTS_RELEASE)
            .collect()
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

    /// Restart this server under a different ack key (a new ack secret,
    /// or the ephemeral keys a non-prod server generates on every boot):
    /// same storage and metadata, a different own guardian key.
    async fn restart_with_a_new_ack_key(&mut self) {
        let keystore_dir =
            std::env::temp_dir().join(format!("guardian_test_keystore_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&keystore_dir).expect("keystore dir");
        self.state.ack = crate::ack::AckRegistry::new(keystore_dir)
            .await
            .expect("ack registry");
        assert_ne!(
            self.state.ack.commitment(&SignatureScheme::Falcon),
            self.ack_commitment_hex,
            "the restarted server holds a different key"
        );
    }

    /// Promote the switch delta the way canonicalization does (state and
    /// canonical delta, its ack signed by this server's key) but without
    /// its push-path release: the release write failed.
    async fn promote_switch_without_its_release(&self) -> u64 {
        let executed_nonce = self.executed_account.nonce().as_canonical_u64();
        let now = chrono::Utc::now().to_rfc3339();
        let mut promoted = self
            .state
            .storage
            .pull_state(&self.account_id_hex)
            .await
            .expect("state readable");
        promoted.commitment = self.executed_commitment.clone();
        promoted.state_json = self.executed_account.to_json();
        promoted.updated_at = now.clone();
        self.state
            .storage
            .submit_state(&promoted)
            .await
            .expect("promoted state stored");
        let acked = self
            .state
            .ack
            .ack_delta(
                DeltaObject {
                    account_id: self.account_id_hex.clone(),
                    nonce: executed_nonce,
                    prev_commitment: self.pre_switch_commitment.clone(),
                    new_commitment: Some(self.executed_commitment.clone()),
                    delta_payload: self.switch_summary.clone(),
                    ack_sig: String::new(),
                    ack_pubkey: self.state.ack.pubkey(&SignatureScheme::Falcon),
                    ack_scheme: "falcon".to_string(),
                    status: crate::delta_object::DeltaStatus::canonical(now),
                    metadata: None,
                },
                &SignatureScheme::Falcon,
            )
            .await
            .expect("this server acks the switch");
        self.state
            .storage
            .submit_delta(&acked)
            .await
            .expect("canonical delta stored");
        executed_nonce
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
    // `MockChain` charges no fee and the guarded component pays none on
    // this protocol line, so the auth args commit no conversion info
    // (see switch_guardian_canonicalization.rs).
    let auth_args = MultisigAuthArgs::new(
        mock_chain.latest_block_header().block_num(),
        Word::from([Felt::new_unchecked(7); 4]),
    );

    // The mock chain holds no state for private accounts, so the
    // transaction is built from the account itself for both visibilities.
    let abort_summary = match mock_chain
        .build_transaction(multisig_account.clone())
        .authenticator(None)
        .tx_script(tx_script.clone())
        .multisig_auth_args(&auth_args)
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
        .multisig_auth_args(&auth_args)
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
    let switch_header = TransactionHeader::from(&executed_tx);
    let switch_tx_initial = format!(
        "0x{}",
        hex::encode(switch_header.initial_state_commitment().as_bytes())
    );
    let switch_tx_final = format!(
        "0x{}",
        hex::encode(switch_header.final_state_commitment().as_bytes())
    );
    assert_eq!(
        switch_tx_final, executed_commitment,
        "the header's final commitment is the executed state"
    );
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
        switch_tx_initial,
        switch_tx_final,
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

    // ...and re-onboarding via /configure reactivates. (The integration
    // client stubs the guardian-binding validation, so the executed state
    // stands in for a state switched back to this server; the clear itself
    // is what is under test.)
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
    let proposal_id = fixture.push_switch_proposal().await;

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
    let release_events = fixture.release_events();
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
    assert_eq!(
        event.payload["switch_commitment"],
        fixture.executed_commitment
    );
    assert!(
        event.payload["switch_block_num"].is_null(),
        "matched at the chain head, no history search"
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

#[tokio::test]
async fn test_switch_proposal_behind_a_stuck_candidate_is_refused_then_releases_once_parked() {
    // A candidate is stuck in this server's queue (issue #17): acknowledged,
    // never landed. The wallet builds the switch on the stored state, so
    // its nonce is the stuck candidate's: its delta could never be
    // admitted (the slot is taken while the candidate is queued, and by
    // the candidate once it promotes), and the proposal is refused up
    // front, as before the queue existed. Once the stuck candidate is
    // parked the same proposal is accepted on the stored state; the
    // switch executes from there, and the sweep releases the private
    // account on that proposal.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    let (stuck_nonce, _stuck_post_state) = fixture.push_stuck_candidate().await;
    let switch_nonce = fixture.executed_account.nonce().as_canonical_u64();
    assert_eq!(switch_nonce, stuck_nonce, "both build on the stored state");
    let refused = fixture
        .try_push_switch_proposal(switch_nonce)
        .await
        .expect_err("a switch at the stuck candidate's nonce is doomed");
    assert!(
        matches!(refused, crate::error::GuardianError::ConflictPendingDelta),
        "expected ConflictPendingDelta, got {refused:?}"
    );
    assert!(
        fixture
            .state
            .storage
            .pull_pending_proposals(&fixture.account_id_hex)
            .await
            .expect("proposals readable")
            .is_empty(),
        "nothing is stored for cosigners to sign"
    );

    fixture.park_diverged_candidate(stuck_nonce).await;
    let proposal_id = fixture.push_switch_proposal().await;
    let recorded = fixture
        .state
        .storage
        .pull_pending_proposals(&fixture.account_id_hex)
        .await
        .expect("proposals readable");
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].proposal.prev_commitment, fixture.pre_switch_commitment,
        "with the queue empty the proposal is pinned to the stored state"
    );

    let executed = fixture.executed_account.clone();
    fixture.install_chain(&executed, false);
    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(fixture.released_at().await.is_some());
    let release_events = fixture.release_events();
    assert_eq!(release_events.len(), 1);
    assert_eq!(release_events[0].payload["detected_by"], "proposal_match");
    assert_eq!(release_events[0].payload["proposal_id"], proposal_id);
}

#[tokio::test]
async fn test_release_sweep_matches_a_switch_proposal_recorded_against_a_stuck_queue_tail() {
    // The nonce check cannot catch a proposal whose nonce extends the
    // queue but whose summary was built on the stored state: nothing in a
    // transaction summary names its base, so the queue records it against
    // its tail, the stuck candidate's post-state. The switch executes on
    // chain from the stored state, and canonicalization parks the stuck
    // candidate once the chain moved off its base. The proposal is still
    // the evidence that releases the private account.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    let (stuck_nonce, stuck_post_state) = fixture.push_stuck_candidate().await;
    let past_the_tail = fixture.executed_account.nonce().as_canonical_u64() + 1;
    let proposal_id = fixture
        .try_push_switch_proposal(past_the_tail)
        .await
        .expect("a nonce past the tail is admitted");
    let recorded = fixture
        .state
        .storage
        .pull_pending_proposals(&fixture.account_id_hex)
        .await
        .expect("proposals readable");
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].proposal.prev_commitment, stuck_post_state,
        "the queue records the proposal against its tail"
    );

    let executed = fixture.executed_account.clone();
    fixture.install_chain(&executed, false);
    fixture.park_diverged_candidate(stuck_nonce).await;

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(
        fixture.released_at().await.is_some(),
        "a proposal recorded against the queue tail must still prove the switch"
    );
    let release_events = fixture.release_events();
    assert_eq!(release_events.len(), 1);
    let event = &release_events[0];
    assert_eq!(event.payload["detected_by"], "proposal_match");
    assert_eq!(event.payload["proposal_id"], proposal_id);
    assert_eq!(
        event.payload["stored_commitment"],
        fixture.pre_switch_commitment
    );
    assert_eq!(
        event.payload["switch_commitment"],
        fixture.executed_commitment
    );
    assert!(
        fixture
            .state
            .storage
            .pull_pending_proposals(&fixture.account_id_hex)
            .await
            .expect("proposals readable")
            .is_empty(),
        "the switch proposal the chain proved executed is finalized"
    );
    let stuck = fixture
        .state
        .storage
        .pull_delta(&fixture.account_id_hex, stuck_nonce)
        .await
        .expect("the parked row is readable");
    assert!(
        stuck.status.is_retained(),
        "the parked candidate is left to the reconcile pass and its TTL"
    );
}

#[tokio::test]
async fn test_release_sweep_finds_the_switch_in_the_history_after_the_account_moved_on() {
    // The new guardian already transacted, so the chain head is past the
    // post-switch state and no head match is possible. The node's
    // transaction history still lists the switch transaction (its real
    // header), whose final commitment is exactly the post-state the
    // pending proposal predicts.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    let proposal_id = fixture.push_switch_proposal().await;
    let moved = fixture.moved_past_switch_account();
    let history = vec![
        (
            40,
            fixture.switch_tx_initial.clone(),
            fixture.switch_tx_final.clone(),
        ),
        (
            41,
            fixture.executed_commitment.clone(),
            commitment_hex(&moved),
        ),
    ];
    fixture.install_chain_with_history(&moved, false, &history);

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(fixture.released_at().await.is_some());
    let release_events = fixture.release_events();
    assert_eq!(release_events.len(), 1);
    let event = &release_events[0];
    assert_eq!(event.payload["detected_by"], "proposal_match");
    assert_eq!(event.payload["proposal_id"], proposal_id);
    assert_eq!(event.payload["on_chain_commitment"], commitment_hex(&moved));
    assert_eq!(
        event.payload["switch_commitment"],
        fixture.executed_commitment
    );
    assert_eq!(event.payload["switch_block_num"], 40);
    assert_eq!(
        event.payload["new_guardian_commitment"],
        fixture.new_guardian_commitment_hex
    );
    assert!(
        fixture
            .state
            .storage
            .pull_pending_proposals(&fixture.account_id_hex)
            .await
            .expect("proposals readable")
            .is_empty()
    );
}

#[tokio::test]
async fn test_release_sweep_proves_a_retained_switch_delta_whose_proposal_is_gone() {
    // The wallet pushed the switch delta to this server, but by the time
    // canonicalization looked the account had already moved on under the
    // new guardian: the candidate was retained and its proposal deleted
    // (what `retain_candidate` does). The retained row still carries the
    // real switch summary, and the history holds the switch transaction.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    let executed_nonce = fixture.executed_account.nonce().as_canonical_u64();
    fixture
        .state
        .storage
        .submit_delta(&DeltaObject {
            account_id: fixture.account_id_hex.clone(),
            nonce: executed_nonce,
            prev_commitment: fixture.pre_switch_commitment.clone(),
            new_commitment: Some(fixture.executed_commitment.clone()),
            delta_payload: fixture.switch_summary.clone(),
            ack_sig: String::new(),
            ack_pubkey: String::new(),
            ack_scheme: String::new(),
            status: crate::delta_object::DeltaStatus::retained(
                chrono::Utc::now().to_rfc3339(),
                crate::delta_object::RetainReason::Diverged,
            ),
            metadata: None,
        })
        .await
        .expect("retained row stored");
    assert!(
        fixture
            .state
            .storage
            .pull_pending_proposals(&fixture.account_id_hex)
            .await
            .expect("proposals readable")
            .is_empty(),
        "no proposal is left as evidence"
    );
    let moved = fixture.moved_past_switch_account();
    let history = vec![
        (
            40,
            fixture.switch_tx_initial.clone(),
            fixture.switch_tx_final.clone(),
        ),
        (
            41,
            fixture.executed_commitment.clone(),
            commitment_hex(&moved),
        ),
    ];
    fixture.install_chain_with_history(&moved, false, &history);

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(fixture.released_at().await.is_some());
    let release_events = fixture.release_events();
    assert_eq!(release_events.len(), 1);
    let event = &release_events[0];
    assert_eq!(event.payload["detected_by"], "recoverable_delta");
    assert_eq!(event.payload["delta_nonce"], executed_nonce);
    assert_eq!(
        event.payload["switch_commitment"],
        fixture.executed_commitment
    );
    assert_eq!(event.payload["switch_block_num"], 40);
    assert_eq!(
        event.payload["new_guardian_commitment"],
        fixture.new_guardian_commitment_hex
    );
}

#[tokio::test]
async fn test_release_sweep_needs_the_exact_post_state_in_the_history() {
    // The account moved on, but no transaction in its history ends at
    // the pending proposal's post-state (the switch never executed; the
    // chain moved for another reason). A private account stays opaque.
    let mut fixture = unannounced_switch_for(AccountType::Private).await;
    fixture.push_switch_proposal().await;
    let moved = fixture.moved_past_switch_account();
    let history = vec![(
        12,
        fixture.pre_switch_commitment.clone(),
        commitment_hex(&moved),
    )];
    fixture.install_chain_with_history(&moved, false, &history);

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(fixture.released_at().await.is_none());
    assert!(fixture.release_events().is_empty());
    assert_eq!(
        fixture
            .state
            .storage
            .pull_pending_proposals(&fixture.account_id_hex)
            .await
            .expect("proposals readable")
            .len(),
        1,
        "an unproven proposal is never finalized"
    );
}

#[tokio::test]
async fn test_release_sweep_ignores_a_storage_read_older_than_the_stored_state() {
    // The account was re-onboarded with a newer state bound to this
    // server (nonce +2), while a lagging node still serves the switched
    // state (nonce +1) with the foreign key. The read is older than what
    // this server stores, so it is never evidence of a switch.
    let mut fixture = unannounced_switch().await;
    let mut newer = fixture.advanced_still_bound_account();
    newer.increment_nonce(Felt::ONE).expect("nonce increments");
    assert!(newer.nonce() > fixture.executed_account.nonce());
    let creds = fixture.credentials();
    configure_account(
        &fixture.state,
        ConfigureAccountParams {
            account_id: fixture.account_id_hex.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: fixture.all_commitments_hex.clone(),
            },
            network_config: NetworkConfig::miden_default(),
            initial_state: newer.to_json(),
            credential: creds,
        },
    )
    .await
    .expect("re-onboarding with the newer state succeeds");

    let executed = fixture.executed_account.clone();
    fixture.install_chain(&executed, true);

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(
        fixture.released_at().await.is_none(),
        "a read older than the stored state must never release"
    );
    assert!(fixture.release_events().is_empty());
}

#[tokio::test]
async fn test_release_sweep_writes_a_push_path_release_that_never_landed() {
    // The switch delta reached this server and was promoted, but the
    // push-path release write failed. The stored state's key and the ack
    // this server signed for that delta prove the switch, even for a
    // private account, whether the chain still sits at the switched state
    // or the account has transacted under the new guardian since.
    for moved_on in [false, true] {
        let mut fixture = unannounced_switch_for(AccountType::Private).await;
        let delta_nonce = fixture.promote_switch_without_its_release().await;
        let on_chain = if moved_on {
            fixture.moved_past_switch_account()
        } else {
            fixture.executed_account.clone()
        };
        fixture.install_chain(&on_chain, false);

        let pass = run_release_sweep_now(&fixture.state)
            .await
            .expect("sweep succeeds");
        assert_eq!(pass.failed_accounts, 0, "moved_on={moved_on}");
        assert!(fixture.released_at().await.is_some(), "moved_on={moved_on}");
        let release_events = fixture.release_events();
        assert_eq!(release_events.len(), 1, "moved_on={moved_on}");
        let event = &release_events[0];
        assert_eq!(event.payload["detected_by"], "delta");
        assert_eq!(event.payload["delta_nonce"], delta_nonce);
        assert_eq!(event.payload["new_commitment"], fixture.executed_commitment);
        assert_eq!(
            event.payload["new_guardian_commitment"],
            fixture.new_guardian_commitment_hex
        );
    }
}

#[tokio::test]
async fn test_release_sweep_releases_nothing_after_this_servers_ack_key_changed() {
    // A server restarted under a different ack key (a new ack secret, or
    // a non-prod server's ephemeral keys) sees every account bound to its
    // previous key. None of them switched: the sweep must not release a
    // healthy account, a lagging one, or one whose promoted delta it
    // acknowledged under the previous key.
    let mut healthy = unannounced_switch().await;
    let pre_switch = healthy.pre_switch_account.clone();
    healthy.install_chain(&pre_switch, true);

    let mut lagging = unannounced_switch().await;
    let advanced = lagging.advanced_still_bound_account();
    lagging.install_chain(&advanced, true);

    let mut acked_before = unannounced_switch_for(AccountType::Private).await;
    acked_before.promote_switch_without_its_release().await;
    let executed = acked_before.executed_account.clone();
    acked_before.install_chain(&executed, false);

    for fixture in [&mut healthy, &mut lagging, &mut acked_before] {
        fixture.restart_with_a_new_ack_key().await;
        let pass = run_release_sweep_now(&fixture.state)
            .await
            .expect("sweep succeeds");
        assert_eq!(pass.failed_accounts, 0);
        assert!(fixture.released_at().await.is_none());
        assert!(fixture.release_events().is_empty());
    }
}

#[tokio::test]
async fn test_release_sweep_still_releases_a_real_switch_after_this_servers_ack_key_changed() {
    // The key change does not blind the sweep: an account that did switch
    // to another guardian carries neither the stored base's key nor this
    // server's new one.
    let mut fixture = unannounced_switch().await;
    let executed = fixture.executed_account.clone();
    fixture.install_chain(&executed, true);
    fixture.restart_with_a_new_ack_key().await;

    let pass = run_release_sweep_now(&fixture.state)
        .await
        .expect("sweep succeeds");
    assert_eq!(pass.failed_accounts, 0);
    assert!(fixture.released_at().await.is_some());
    let release_events = fixture.release_events();
    assert_eq!(release_events.len(), 1);
    assert_eq!(release_events[0].payload["detected_by"], "chain_sweep");
    assert_eq!(
        release_events[0].payload["new_guardian_commitment"],
        fixture.new_guardian_commitment_hex
    );
}
