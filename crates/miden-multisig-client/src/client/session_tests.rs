//! Guardian sessions on the multisig client, against the in-process mock
//! GUARDIAN gRPC server from `guardian_client::testing`.
use std::sync::Arc;
use std::time::Duration;

use guardian_client::StartSessionOptions;
use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use miden_protocol::Word;
use tonic::Status;

use super::test_support::{
    chain_with_notes, multisig_account, offline_client_parts_with_keystore, registered_state,
};
use crate::account::MultisigAccount;
use crate::client::MultisigClient;
use crate::keystore::{GuardianKeyStore, KeyManager};

const GUARDIAN_COMMITMENT: &str =
    "0x2222222222222222222222222222222222222222222222222222222222222222";

fn options() -> StartSessionOptions {
    StartSessionOptions {
        network: "devnet".to_string(),
        ttl: Duration::from_secs(3600),
    }
}

/// A rejected session, as Guardian reports it for a key it no longer knows.
fn rejected() -> Status {
    let details = serde_json::json!({
        "code": "authentication_failed",
        "message": "unknown session key",
        "meta": { "retryable": false }
    });
    Status::with_details(
        tonic::Code::Unauthenticated,
        "unknown session key",
        details.to_string().into_bytes().into(),
    )
}

/// A synced client whose account Guardian serves, connected to `service`.
async fn connected_client(
    dir: &std::path::Path,
    service: MockGuardianService,
) -> (MultisigClient, miden_protocol::account::Account) {
    let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let account = multisig_account(keystore.commitment(), Word::from([9u32, 9, 9, 9]), 7);
    let api = chain_with_notes(vec![]);
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir, api.clone(), None, keystore).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, false).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    let handle = service.handle();
    handle.set_persistent_get_state(registered_state(&account));
    handle.set_persistent_get_pubkey(GUARDIAN_COMMITMENT.to_string());
    let endpoint = start_mock_server(service).await.unwrap();
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    (client, account)
}

#[tokio::test]
async fn operations_sign_with_the_session_until_it_ends() {
    let dir = tempfile::tempdir().unwrap();
    let service = MockGuardianService::default();
    let get_state_auth = service.get_state_auth_formats_handle();
    let logouts = service.revoke_session_auth_handle();
    let (mut client, account) = connected_client(dir.path(), service).await;

    let info = client.start_session(options()).await.unwrap();
    assert_eq!(client.session(), Some(info.clone()));

    client.pull_account(account.id()).await.unwrap();
    client.pull_account(account.id()).await.unwrap();
    let signed = get_state_auth.lock().unwrap().clone();
    assert_eq!(signed.len(), 2);
    for (auth_format, pubkey) in &signed {
        assert_eq!(auth_format, "session", "each operation reuses the session");
        assert_eq!(pubkey, &info.session_public_key);
    }

    assert!(client.end_session().await.unwrap());
    assert_eq!(logouts.lock().unwrap()[0].0, info.session_public_key);
    assert!(client.session().is_none());
    client.pull_account(account.id()).await.unwrap();
    assert_eq!(
        get_state_auth.lock().unwrap()[2].0,
        "",
        "back to the key manager"
    );
}

#[tokio::test]
async fn a_session_guardian_rejects_is_dropped_for_every_later_operation() {
    let dir = tempfile::tempdir().unwrap();
    let service = MockGuardianService::default().with_get_state(Err(rejected()));
    let (mut client, account) = connected_client(dir.path(), service).await;
    client.start_session(options()).await.unwrap();

    client
        .pull_account(account.id())
        .await
        .expect_err("Guardian no longer knows the session key");

    assert!(client.session().is_none());
}

#[tokio::test]
async fn starting_a_session_logs_out_the_one_it_replaces() {
    let dir = tempfile::tempdir().unwrap();
    let service = MockGuardianService::default();
    let logouts = service.revoke_session_auth_handle();
    let (mut client, _account) = connected_client(dir.path(), service).await;

    let first = client.start_session(options()).await.unwrap();
    let second = client.start_session(options()).await.unwrap();

    let logged_out: Vec<String> = logouts
        .lock()
        .unwrap()
        .iter()
        .map(|(pubkey, _, _)| pubkey.clone())
        .collect();
    assert_eq!(logged_out, vec![first.session_public_key]);
    assert_eq!(client.session(), Some(second));
}

#[tokio::test]
async fn revoke_all_and_an_endpoint_change_end_the_session() {
    let dir = tempfile::tempdir().unwrap();
    let service = MockGuardianService::default();
    let revoke_all_calls = service.revoke_all_sessions_calls_handle();
    let (mut client, _account) = connected_client(dir.path(), service).await;

    client.start_session(options()).await.unwrap();
    client.revoke_all_sessions().await.unwrap();
    assert_eq!(revoke_all_calls.lock().unwrap().len(), 1);
    assert!(client.session().is_none());

    client.start_session(options()).await.unwrap();
    let endpoint = start_mock_server(MockGuardianService::default())
        .await
        .unwrap();
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    assert!(
        client.session().is_none(),
        "the session belongs to the old Guardian"
    );
}
