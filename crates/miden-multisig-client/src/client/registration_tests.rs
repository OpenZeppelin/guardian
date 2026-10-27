//! Registration on GUARDIAN against the in-process mock GUARDIAN gRPC
//! server from `guardian_client::testing`.
use std::sync::Arc;

use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use miden_protocol::Word;
use tonic::Status;

use super::test_support::{chain_with_notes, multisig_account, offline_client_parts_with_keystore};
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

/// A client holding a multisig account, connected to `service`.
async fn connected_client(dir: &std::path::Path, service: MockGuardianService) -> MultisigClient {
    let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let account = multisig_account(keystore.commitment(), Word::from([9u32, 9, 9, 9]), 7);
    let api = chain_with_notes(vec![]);
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

#[tokio::test]
async fn registration_waits_for_the_state_to_reach_the_chain() {
    let dir = tempfile::tempdir().unwrap();
    let service = MockGuardianService::default().with_configure(Err(refused(
        "state_not_on_chain",
        tonic::Code::FailedPrecondition,
        true,
    )));
    let handle = service.handle();
    let mut client = connected_client(dir.path(), service).await;

    client.register_on_guardian().await.unwrap();

    let configures = handle
        .calls()
        .into_iter()
        .filter(|call| call == "configure")
        .count();
    assert_eq!(configures, 2);
}

#[tokio::test]
async fn registration_does_not_retry_any_other_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let service = MockGuardianService::default().with_configure(Err(refused(
        "commitment_mismatch",
        tonic::Code::FailedPrecondition,
        false,
    )));
    let handle = service.handle();
    let mut client = connected_client(dir.path(), service).await;

    let result = client.register_on_guardian().await;

    assert!(result.is_err(), "{result:?}");
    let configures = handle
        .calls()
        .into_iter()
        .filter(|call| call == "configure")
        .count();
    assert_eq!(configures, 1);
}
