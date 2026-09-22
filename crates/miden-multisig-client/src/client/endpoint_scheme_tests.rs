//! Endpoint-commitment scheme binding (issue #432), driven against the
//! in-process mock GUARDIAN gRPC servers from `guardian_client::testing`.
//!
//! GUARDIAN holds one acknowledgement identity per signature scheme and
//! serves its default when asked without one, so `verify_endpoint_commitment`
//! must name the account's own scheme or it compares the account's stored
//! guardian commitment against a different identity. The parameter's type
//! forces a caller to pass *a* scheme; only these tests establish that the
//! call sites pass the account's, which is why each covered site is exercised
//! with an ECDSA account as well as a Falcon one. The TypeScript client
//! asserts the same binding over its query string in `multisig.test.ts`.
use std::sync::Arc;

use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use guardian_shared::SignatureScheme;
use miden_protocol::Word;

use super::test_support::{
    chain_with_notes, multisig_account_with_scheme, offline_client_parts_with_keystore,
    registered_state,
};
use crate::account::MultisigAccount;
use crate::client::MultisigClient;
use crate::execution::build_final_transaction_request;
use crate::keystore::{EcdsaGuardianKeyStore, GuardianKeyStore, KeyManager};
use crate::proposal::TransactionType;
use crate::transaction::{execute_for_summary, generate_salt, word_to_hex};

/// The commitment the switch target serves, and the one the proposal names,
/// so the check passes for either scheme: what these tests observe is which
/// identity was asked for, not whether the comparison succeeded.
const NEW_GUARDIAN_COMMITMENT: [u32; 4] = [11, 12, 13, 14];

/// A synced client whose account and signer both use `scheme`, plus the
/// account itself for seeding a mock GUARDIAN's state response.
async fn client_for_scheme(
    dir: &std::path::Path,
    scheme: SignatureScheme,
    seed: u8,
) -> (MultisigClient, miden_protocol::account::Account) {
    let keystore: Arc<dyn KeyManager> = match scheme {
        SignatureScheme::Falcon => Arc::new(GuardianKeyStore::generate()),
        SignatureScheme::Ecdsa => Arc::new(EcdsaGuardianKeyStore::generate()),
    };
    let account = multisig_account_with_scheme(
        keystore.commitment(),
        Word::from([9u32, 9, 9, 9]),
        seed,
        scheme,
    );

    let api = chain_with_notes(vec![]);
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir, api.clone(), None, keystore).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, false).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();
    (client, account)
}

/// Starts the mock switch target, serving the commitment the proposal names.
async fn switch_target() -> (String, guardian_client::testing::mocks::MockGuardianHandle) {
    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_pubkey(word_to_hex(&Word::from(NEW_GUARDIAN_COMMITMENT)));
    (endpoint, handle)
}

fn assert_queried_only(schemes: &[Option<String>], expected: SignatureScheme, site: &str) {
    assert!(
        !schemes.is_empty(),
        "{site} must check the endpoint commitment"
    );
    for scheme in schemes {
        assert_eq!(
            scheme.as_deref(),
            Some(expected.as_str()),
            "{site} must query GUARDIAN's {} identity, not the server default; got {schemes:?}",
            expected.as_str()
        );
    }
}

/// Offline creation (`MultisigClient::create_proposal_offline`).
async fn offline_creation_queries(scheme: SignatureScheme, seed: u8) -> Vec<Option<String>> {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, _account) = client_for_scheme(dir.path(), scheme, seed).await;
    let (endpoint, handle) = switch_target().await;

    client
        .create_proposal_offline(TransactionType::switch_guardian(
            endpoint,
            Word::from(NEW_GUARDIAN_COMMITMENT),
        ))
        .await
        .expect("switch proposal is created");

    handle.get_pubkey_schemes()
}

/// Online creation (`ProposalBuilder::build_switch_guardian`), against a mock
/// current GUARDIAN. The build is allowed to fail past the endpoint check:
/// the mock's canned push response carries no matching commitment, and the
/// call under test has already happened by then.
async fn online_creation_queries(scheme: SignatureScheme, seed: u8) -> Vec<Option<String>> {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, account) = client_for_scheme(dir.path(), scheme, seed).await;
    let (target_endpoint, target_handle) = switch_target().await;

    let current = MockGuardianService::default();
    let current_handle = current.handle();
    let current_endpoint = start_mock_server(current).await.unwrap();
    current_handle.set_persistent_get_state(registered_state(&account));
    client
        .set_guardian_endpoint(&current_endpoint, false)
        .await
        .unwrap();

    let _ = client
        .propose_transaction(TransactionType::switch_guardian(
            target_endpoint,
            Word::from(NEW_GUARDIAN_COMMITMENT),
        ))
        .await;

    target_handle.get_pubkey_schemes()
}

/// Execution finalization (`MultisigClient::finalize_transaction`), the
/// third call site. Driven directly with a real switch request rather than
/// through `execute_proposal`, which needs a fully signed proposal and dies
/// past this point on the mock's transaction encryption key. The endpoint
/// check is the first thing `finalize_transaction` does, so the later
/// failure is immaterial.
async fn finalization_queries(scheme: SignatureScheme, seed: u8) -> Vec<Option<String>> {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, account) = client_for_scheme(dir.path(), scheme, seed).await;
    let (endpoint, handle) = switch_target().await;

    let tx_type = TransactionType::switch_guardian(endpoint, Word::from(NEW_GUARDIAN_COMMITMENT));
    let auth_args = client
        .multisig_auth_args(generate_salt(), None, None)
        .await
        .unwrap();
    let tx_request = build_final_transaction_request(
        &client.miden_client,
        &tx_type,
        &account,
        &auth_args,
        Vec::new(),
        None,
        Some(&[]),
        scheme,
    )
    .await
    .expect("switch request builds");
    let (_summary, chain_anchor) =
        execute_for_summary(&mut client.miden_client, account.id(), tx_request.clone())
            .await
            .expect("switch request executes for a summary");

    let _ = client
        .finalize_transaction(account.id(), tx_request, &tx_type, chain_anchor)
        .await;

    handle.get_pubkey_schemes()
}

#[tokio::test]
async fn offline_switch_creation_queries_the_falcon_identity() {
    let schemes = offline_creation_queries(SignatureScheme::Falcon, 61).await;
    assert_queried_only(&schemes, SignatureScheme::Falcon, "offline creation");
}

#[tokio::test]
async fn offline_switch_creation_queries_the_ecdsa_identity() {
    let schemes = offline_creation_queries(SignatureScheme::Ecdsa, 67).await;
    assert_queried_only(&schemes, SignatureScheme::Ecdsa, "offline creation");
}

#[tokio::test]
async fn online_switch_creation_queries_the_falcon_identity() {
    let schemes = online_creation_queries(SignatureScheme::Falcon, 71).await;
    assert_queried_only(&schemes, SignatureScheme::Falcon, "online creation");
}

#[tokio::test]
async fn online_switch_creation_queries_the_ecdsa_identity() {
    let schemes = online_creation_queries(SignatureScheme::Ecdsa, 73).await;
    assert_queried_only(&schemes, SignatureScheme::Ecdsa, "online creation");
}

#[tokio::test]
async fn switch_finalization_queries_the_falcon_identity() {
    let schemes = finalization_queries(SignatureScheme::Falcon, 79).await;
    assert_queried_only(&schemes, SignatureScheme::Falcon, "finalization");
}

#[tokio::test]
async fn switch_finalization_queries_the_ecdsa_identity() {
    let schemes = finalization_queries(SignatureScheme::Ecdsa, 83).await;
    assert_queried_only(&schemes, SignatureScheme::Ecdsa, "finalization");
}
