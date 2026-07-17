//! End-to-end confirmation of the issue #305 mechanism.
//!
//! Since #287, the multisig client's `execute_proposal` pushes a
//! `SwitchGuardian` delta (the abort `TransactionSummary`) to the
//! **pre-switch** guardian via the regular `push_delta` path. This test
//! reproduces that flow against a real mock chain and asserts the pushed
//! delta canonicalizes on the old guardian:
//!
//! 1. Build a real 2-of-2 multisig account whose guardian key is THIS
//!    server's ack key — the same binding `/configure` enforces in
//!    production.
//! 2. Execute `update_guardian_public_key` on a mock chain twice, exactly
//!    like `test_switch_guardian_server_reconstruction_matches_execution`
//!    (crates/contracts/tests/auth/multisig.rs): an abort run to obtain the
//!    summary the wallet pushes, and a signed run for the authoritative
//!    on-chain result.
//! 3. Configure the account on the server, push the switch delta with the
//!    client's exact payload shape, and run the canonicalization worker with
//!    the executed commitment registered as the on-chain answer.
//! 4. Assert the delta canonicalizes, the stored state converges to the
//!    executed post-switch commitment, and the stored state's guardian
//!    public key no longer matches this server's ack key.
//! 5. Assert the release-on-switch reaction: the account transitions to
//!    `released` (mutations refused, reads still served) and re-onboarding
//!    via `/configure` reactivates it.

use std::sync::Arc;

use guardian_shared::auth_request_message::AuthRequestMessage;
use guardian_shared::auth_request_payload::AuthRequestPayload;
use guardian_shared::hex::IntoHex;
use guardian_shared::{FromJson, SignatureScheme, ToJson};
use miden_confidential_contracts::masm_builder::get_guardian_library;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::account::{Account, AccountType};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::utils::serde::{Deserializable, Serializable};
use miden_protocol::vm::AdviceInputs;
use miden_protocol::{Felt, Word};
use miden_standards::code_builder::CodeBuilder;
use miden_testing::MockChainBuilder;
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};

use crate::delta_object::DeltaObject;
use crate::metadata::NetworkConfig;
use crate::metadata::auth::{Auth, Credentials};
use crate::network::NetworkType;
use crate::network::miden::MidenNetworkClient;
use crate::network::miden::account_inspector::MidenAccountInspector;
use crate::services::{
    ConfigureAccountParams, PushDeltaParams, configure_account, process_canonicalizations_now,
    push_delta,
};
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

#[tokio::test]
async fn test_switch_guardian_delta_canonicalizes_and_releases_on_old_guardian() {
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
        .with_account_type(AccountType::Public)
        .with_guardian_enabled(true)
        .with_signature_scheme(SignatureScheme::Falcon);
    let multisig_account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");

    let pre_inspector = MidenAccountInspector::new(&multisig_account);
    assert_eq!(
        pre_inspector.extract_guardian_public_key().as_deref(),
        Some(ack_commitment_hex.as_str()),
        "pre-switch state must carry this server's guardian key"
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

    let advice_inputs =
        AdviceInputs::default().with_stack(new_guardian_commitment.as_elements().iter().copied());
    let guardian_library = get_guardian_library().expect("guardian library compiles");
    let tx_script_code = r#"
    use oz_guardian::guardian
    begin
        call.guardian::update_guardian_public_key
    end
    "#;
    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(&guardian_library)
        .expect("library links")
        .compile_tx_script(tx_script_code)
        .expect("tx script compiles");
    let salt = Word::from([Felt::new_unchecked(7); 4]);

    // No-signature execution: the TransactionSummary the wallet pushes to the
    // pre-switch guardian as the delta payload.
    let abort_summary = match mock_chain
        .build_tx_context(multisig_account.id(), &[], &[])
        .expect("tx context builds")
        .authenticator(None)
        .tx_script(tx_script.clone())
        .extend_advice_inputs(advice_inputs.clone())
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
    // The exact payload shape the client pushes: `tx_summary.to_json()`.
    let delta_payload = abort_summary.as_ref().to_json();

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

    // Real signed execution: the authoritative on-chain result.
    let executed_tx = mock_chain
        .build_tx_context(multisig_account.id(), &[], &[])
        .expect("tx context builds")
        .authenticator(None)
        .tx_script(tx_script)
        .add_signature(cosigner_pubkeys[0].clone().into(), msg, sig_1)
        .add_signature(cosigner_pubkeys[1].clone().into(), msg, sig_2)
        .auth_args(salt)
        .extend_advice_inputs(advice_inputs)
        .build()
        .expect("tx builds")
        .execute()
        .await
        .expect("signed switch executes");

    let mut executed_account = multisig_account.clone();
    executed_account
        .apply_delta(executed_tx.account_delta())
        .expect("executed delta applies");
    let executed_commitment_hex = commitment_hex(&executed_account);
    let executed_nonce = executed_account.nonce().as_canonical_u64();

    // Network client with real commitment computation; the executed
    // post-switch commitment is the registered on-chain answer.
    let miden_client = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
    let mut integration_client = IntegrationMockNetworkClient::new(miden_client);
    integration_client.register_account(account_id_hex.clone(), executed_commitment_hex.clone());
    state.network_client = Arc::new(integration_client);

    // Capture audit emissions so the release event can be asserted.
    let auditor = CapturingAuditor::new();
    state.auditor = Arc::new(auditor.clone());

    // Onboard the account on this (soon to be old) guardian.
    let api_pubkey_hex = cosigner_pubkeys[0].clone().into_hex();
    let api_commitment_hex = format!(
        "0x{}",
        hex::encode(cosigner_pubkeys[0].to_commitment().to_bytes())
    );
    let timestamp = chrono::Utc::now().timestamp_millis();

    configure_account(
        &state,
        ConfigureAccountParams {
            account_id: account_id_hex.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![api_commitment_hex],
            },
            network_config: NetworkConfig::miden_default(),
            initial_state: multisig_account.to_json(),
            credential: falcon_credentials(
                &cosigner_keys[0],
                &api_pubkey_hex,
                &account_id_hex,
                timestamp,
            ),
        },
    )
    .await
    .expect("configure_account succeeds");

    // Push the switch delta exactly as `execute_proposal` does client-side.
    let push_result = push_delta(
        &state,
        PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id_hex.clone(),
                nonce: executed_nonce,
                prev_commitment: pre_switch_commitment.clone(),
                new_commitment: None,
                delta_payload,
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: Default::default(),
                metadata: None,
            },
            credentials: falcon_credentials(
                &cosigner_keys[0],
                &api_pubkey_hex,
                &account_id_hex,
                timestamp + 1,
            ),
        },
    )
    .await
    .expect("push_delta accepts the switch delta");

    assert!(
        push_result.delta.status.is_candidate(),
        "delta should await canonicalization, got {:?}",
        push_result.delta.status
    );

    let pass = process_canonicalizations_now(&state)
        .await
        .expect("canonicalization run succeeds");
    assert_eq!(
        pass.failed_accounts, 0,
        "no account may fail canonicalization in this pass"
    );
    assert!(!pass.cancelled, "pass must run to completion");

    let deltas = state
        .storage
        .pull_deltas_after(&account_id_hex, 0)
        .await
        .expect("deltas readable");
    let switch_delta = deltas
        .iter()
        .find(|d| d.nonce == executed_nonce)
        .expect("switch delta stored");
    assert!(
        switch_delta.status.is_canonical(),
        "switch delta should canonicalize against the executed on-chain \
         commitment, got {:?}",
        switch_delta.status
    );

    let final_state = state
        .storage
        .pull_state(&account_id_hex)
        .await
        .expect("state readable");
    assert_eq!(
        final_state.commitment, executed_commitment_hex,
        "old guardian's state must converge to the executed post-switch commitment"
    );

    // The predicate the release-on-switch transition keys off: the stored
    // state's guardian key is no longer this server's ack key.
    let final_account =
        Account::from_json(&final_state.state_json).expect("stored state deserializes");
    let post_inspector = MidenAccountInspector::new(&final_account);
    let guardian_after = post_inspector
        .extract_guardian_public_key()
        .expect("guardian pubkey slot present");
    assert_eq!(
        guardian_after, new_guardian_commitment_hex,
        "post-switch state must carry the NEW guardian's key"
    );
    assert_ne!(
        guardian_after, ack_commitment_hex,
        "post-switch guardian key must differ from this server's ack key"
    );

    // Release-on-switch (issue #305): canonicalizing the switch delta must
    // have transitioned the account to `released`.
    let metadata = state
        .metadata
        .get(&account_id_hex)
        .await
        .expect("metadata readable")
        .expect("metadata present");
    assert!(
        metadata.released_at.is_some(),
        "canonicalized guardian switch must release the account"
    );

    // The release must be recorded on the audit trail.
    let release_events: Vec<_> = auditor
        .snapshot()
        .into_iter()
        .filter(|e| e.action_kind == crate::audit::kinds::ACCOUNTS_RELEASE)
        .collect();
    assert_eq!(
        release_events.len(),
        1,
        "release must emit exactly one accounts.release audit event"
    );
    assert_eq!(
        release_events[0].target_account_id.as_deref(),
        Some(account_id_hex.as_str())
    );
    assert_eq!(
        release_events[0].payload["new_guardian_commitment"],
        new_guardian_commitment_hex
    );

    // Mutations are refused with the release-specific error...
    let rejected = push_delta(
        &state,
        PushDeltaParams {
            delta: DeltaObject {
                account_id: account_id_hex.clone(),
                nonce: executed_nonce + 1,
                prev_commitment: executed_commitment_hex.clone(),
                new_commitment: None,
                delta_payload: serde_json::json!({}),
                ack_sig: String::new(),
                ack_pubkey: String::new(),
                ack_scheme: String::new(),
                status: Default::default(),
                metadata: None,
            },
            credentials: falcon_credentials(
                &cosigner_keys[0],
                &api_pubkey_hex,
                &account_id_hex,
                timestamp + 2,
            ),
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

    // ...while reads keep working so the wallet/operator can fetch final state.
    let readable = state
        .storage
        .pull_state(&account_id_hex)
        .await
        .expect("released account state must remain readable");
    assert_eq!(readable.commitment, executed_commitment_hex);

    // Re-onboarding via /configure reactivates the account (the switch-back
    // case). Guardian-binding validation is stubbed Ok by the integration
    // client, mirroring a wallet that switched back to this server.
    configure_account(
        &state,
        ConfigureAccountParams {
            account_id: account_id_hex.clone(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec![format!(
                    "0x{}",
                    hex::encode(cosigner_pubkeys[0].to_commitment().to_bytes())
                )],
            },
            network_config: NetworkConfig::miden_default(),
            initial_state: executed_account.to_json(),
            credential: falcon_credentials(
                &cosigner_keys[0],
                &api_pubkey_hex,
                &account_id_hex,
                timestamp + 3,
            ),
        },
    )
    .await
    .expect("re-onboarding a released account succeeds");

    let metadata = state
        .metadata
        .get(&account_id_hex)
        .await
        .expect("metadata readable")
        .expect("metadata present");
    assert!(
        metadata.released_at.is_none(),
        "re-onboarding via /configure must clear the released state"
    );
}
