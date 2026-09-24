//! Canonical-nonce pre-check on sync (issue #191), driven against the
//! in-process mock GUARDIAN gRPC server and a mock chain.
//!
//! `sync()` asks GUARDIAN for the nonce of its canonical state before
//! pulling the state itself. When GUARDIAN is not ahead of the local
//! account, the state fetch is skipped; when it is ahead, the existing
//! full sync runs unchanged.

use std::sync::Arc;

use guardian_client::GetCanonicalNonceResponse;
use guardian_client::testing::mocks::{MockGuardianHandle, MockGuardianService, start_mock_server};
use miden_protocol::Word;
use tonic::Status;

use super::MultisigClient;
use super::test_support::{
    chain_with_notes, multisig_account, offline_client_parts_with_keystore, registered_state,
};
use crate::account::MultisigAccount;
use crate::keystore::{GuardianKeyStore, KeyManager};
use crate::transaction::word_to_hex;

/// A synced client whose account is registered on a mock GUARDIAN that
/// serves the client's own state, plus the mock's handle. With nothing
/// else armed, the mock derives its canonical nonce from that state, so
/// GUARDIAN and the client agree on the nonce.
async fn synced_client(
    dir: &std::path::Path,
) -> (
    MultisigClient,
    miden_protocol::account::Account,
    MockGuardianHandle,
    MockGuardianService,
) {
    let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let account = multisig_account(keystore.commitment(), Word::from([9u32, 9, 9, 9]), 7);

    let api = chain_with_notes(vec![]);
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir, api.clone(), None, keystore).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, false).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    let service = MockGuardianService::default();
    let handle = service.handle();
    handle.set_persistent_get_state(registered_state(&account));
    (client, account, handle, service)
}

async fn connect(client: &mut MultisigClient, service: MockGuardianService) {
    let endpoint = start_mock_server(service).await.unwrap();
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
}

fn head(account: &miden_protocol::account::Account, nonce: u64) -> GetCanonicalNonceResponse {
    GetCanonicalNonceResponse {
        success: true,
        message: String::new(),
        account_id: account.id().to_string(),
        nonce,
        commitment: word_to_hex(&account.to_commitment()),
        error_code: String::new(),
    }
}

#[tokio::test]
async fn sync_skips_the_state_fetch_when_guardian_is_not_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, account, handle, service) = synced_client(dir.path()).await;
    let local_nonce = account.nonce().as_canonical_u64();
    let service = service
        .with_get_canonical_nonce(Ok(head(&account, local_nonce)))
        .with_get_canonical_nonce(Ok(head(&account, local_nonce.saturating_sub(1))));
    connect(&mut client, service).await;

    client.sync().await.expect("sync at the same nonce");
    client.sync().await.expect("sync when GUARDIAN is behind");

    assert_eq!(
        handle.calls(),
        vec![
            "get_canonical_nonce".to_string(),
            "get_canonical_nonce".to_string()
        ],
        "neither sync may fetch the state when GUARDIAN is not ahead"
    );
    assert_eq!(
        client.account().unwrap().commitment(),
        account.to_commitment(),
        "local state is kept"
    );
}

#[tokio::test]
async fn sync_runs_the_full_state_fetch_when_guardian_is_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, account, handle, service) = synced_client(dir.path()).await;
    let local_nonce = account.nonce().as_canonical_u64();
    let service = service.with_get_canonical_nonce(Ok(head(&account, local_nonce + 1)));
    connect(&mut client, service).await;

    // The mock still serves the registered (same-commitment) state, so the
    // full flow finds nothing newer to import: what this checks is that the
    // pre-check hands over to the state fetch when GUARDIAN reports a
    // higher nonce.
    client.sync().await.expect("sync when GUARDIAN is ahead");

    assert_eq!(
        handle.calls(),
        vec!["get_canonical_nonce".to_string(), "get_state".to_string()],
        "a higher canonical nonce must fall through to the full state fetch"
    );
}

#[tokio::test]
async fn sync_falls_through_to_the_state_fetch_when_the_same_nonce_carries_another_commitment() {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, account, handle, service) = synced_client(dir.path()).await;
    let local_nonce = account.nonce().as_canonical_u64();
    let diverged = GetCanonicalNonceResponse {
        commitment: word_to_hex(&Word::from([1u32, 2, 3, 4])),
        ..head(&account, local_nonce)
    };
    let service = service.with_get_canonical_nonce(Ok(diverged));
    connect(&mut client, service).await;

    // Same nonce, different commitment: not "nothing to pull" but divergence,
    // so the pre-check must not skip; the full flow then reconciles it (today
    // it keeps local, as before this change).
    client.sync().await.expect("sync at a diverged head");

    assert_eq!(
        handle.calls(),
        vec!["get_canonical_nonce".to_string(), "get_state".to_string()],
        "a diverged head at the local nonce must fall through to the full state fetch"
    );
}

#[tokio::test]
async fn sync_derives_the_nonce_from_the_served_state_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, _account, handle, service) = synced_client(dir.path()).await;
    connect(&mut client, service).await;

    client
        .sync()
        .await
        .expect("sync against a mock with no armed nonce");

    assert_eq!(
        handle.calls(),
        vec!["get_canonical_nonce".to_string()],
        "the mock derives GUARDIAN's nonce from the served state, which matches local"
    );
}

#[tokio::test]
async fn sync_surfaces_a_failed_nonce_pre_check() {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, _account, handle, service) = synced_client(dir.path()).await;
    let service = service.with_get_canonical_nonce(Err(Status::unavailable("state undecodable")));
    connect(&mut client, service).await;

    let err = client
        .sync()
        .await
        .expect_err("a failed pre-check is a sync failure, not a silent full fetch");

    assert!(
        err.to_string().contains("canonical nonce"),
        "error should name the pre-check: {err}"
    );
    assert_eq!(
        handle.calls(),
        vec!["get_canonical_nonce".to_string()],
        "no state fetch after a failed pre-check"
    );
}
