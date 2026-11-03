//! Registration on GUARDIAN against the in-process mock GUARDIAN gRPC
//! server from `guardian_client::testing`.
use std::sync::Arc;

use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use miden_protocol::Word;
use tonic::Status;

use super::test_support::{
    chain_holding, chain_with_notes, multisig_account, offline_client_parts_with_keystore,
};
use crate::account::MultisigAccount;
use crate::client::MultisigClient;
use crate::keystore::{GuardianKeyStore, KeyManager};

/// A refusal as GUARDIAN reports it under `code`.
fn refused(code: &str, grpc_code: tonic::Code, retryable: bool) -> Status {
    let details = serde_json::json!({
        "code": code,
        "message": code,
        "meta": { "retryable": retryable }
    });
    Status::with_details(grpc_code, code, details.to_string().into_bytes().into())
}

/// A client holding a multisig account, connected to `service`. With
/// `on_chain`, the chain holds the account's state.
async fn connected_client(
    dir: &std::path::Path,
    service: MockGuardianService,
    on_chain: bool,
) -> MultisigClient {
    let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let account = multisig_account(keystore.commitment(), Word::from([9u32, 9, 9, 9]), 7);
    let api = match on_chain {
        true => chain_holding(account.clone()),
        false => chain_with_notes(vec![]),
    };
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir, api.clone(), None, keystore).await;
    client.set_node_rpc_client(api);
    client.account = Some(MultisigAccount::new(account));
    let endpoint = start_mock_server(service).await.unwrap();
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    client
}

/// A refusal, `state_not_on_chain` included, goes back to the caller after
/// one attempt: every attempt is a wallet signature.
#[tokio::test]
async fn registration_does_not_retry_a_refusal() {
    for code in ["state_not_on_chain", "commitment_mismatch"] {
        let dir = tempfile::tempdir().unwrap();
        let service = MockGuardianService::default().with_configure(Err(refused(
            code,
            tonic::Code::FailedPrecondition,
            code == "state_not_on_chain",
        )));
        let handle = service.handle();
        let mut client = connected_client(dir.path(), service, true).await;

        let result = client.register_on_guardian().await;

        assert!(
            matches!(&result, Err(e) if e.to_string().contains(code)),
            "{code}: {result:?}"
        );
        let configures = handle
            .calls()
            .into_iter()
            .filter(|call| call == "configure")
            .count();
        assert_eq!(configures, 1, "{code}");
    }
}

/// A guardian switch waits for the chain to hold the switched state before
/// it registers on the new GUARDIAN; the wait ends as soon as it does.
#[tokio::test]
async fn the_wait_for_a_state_ends_once_the_chain_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let client = connected_client(dir.path(), MockGuardianService::default(), true).await;
    let account = client.account.as_ref().unwrap();

    client
        .wait_for_on_chain_state(account.id(), account.commitment())
        .await
        .unwrap();
}

/// A state that never reaches the chain ends the wait with an error, after
/// chain reads only, so the switch never asks the wallet to sign a
/// registration GUARDIAN would refuse.
#[tokio::test(start_paused = true)]
async fn the_wait_for_a_state_gives_up_when_it_never_reaches_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let service = MockGuardianService::default();
    let handle = service.handle();
    let client = connected_client(dir.path(), service, false).await;
    let account = client.account.as_ref().unwrap();

    let started = tokio::time::Instant::now();
    let result = client
        .wait_for_on_chain_state(account.id(), account.commitment())
        .await;

    assert!(
        matches!(&result, Err(e) if e.to_string().contains("has not reached the chain")),
        "{result:?}"
    );
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(40),
        "the wait backs off through every poll, about 45 seconds: {:?}",
        started.elapsed()
    );
    assert!(!handle.calls().iter().any(|call| call == "configure"));
}
