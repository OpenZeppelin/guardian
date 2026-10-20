//! Sync rolls back a local state whose transaction will not land.
//!
//! Membership, threshold and switch transactions are written to the store
//! without a miden-client record, so miden-client cannot undo one the node
//! drops. These tests put such a state in the store, ahead of GUARDIAN and
//! the chain, and check when sync returns the device to GUARDIAN's state:
//! only once this client's submission of it is dropped by miden-client's own
//! rule (expired, or stale), the chain sits at GUARDIAN's state, and GUARDIAN
//! does not hold the local state as a delta that may still land. GUARDIAN's
//! absence alone is no evidence: it can forget a delta whose transaction is
//! still pending.

use std::sync::Arc;

use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use guardian_client::{
    DeltaObject as ProtoDeltaObject, DeltaStatus, GetDeltaResponse, delta_status,
};
use miden_client::testing::MockChainBuilder;
use miden_client::testing::mock::MockRpcApi;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::Account;
use miden_protocol::block::BlockNumber;
use miden_protocol::{Felt, Word};
use tonic::Status;

use super::test_support::{offline_client_parts_with_keystore, registered_state};
use super::{MultisigClient, UntrackedSubmission};
use crate::account::MultisigAccount;
use crate::keystore::{GuardianKeyStore, KeyManager};
use crate::transaction::word_to_hex;

/// GUARDIAN's canonical state, the chain's view of it, and a client whose
/// store holds `local`: that state advanced by one transaction, as
/// `finalize_transaction` writes it, recorded as submitted and already
/// expired at the node.
struct Ahead {
    client: MultisigClient,
    guardian_state: Account,
    local: Account,
    service: MockGuardianService,
}

async fn client_ahead_of_guardian(dir: &std::path::Path, chain_holds_local: bool) -> Ahead {
    client_ahead_of_guardian_at(dir, Some(chain_holds_local), 0).await
}

/// As [`client_ahead_of_guardian`], on a chain proved up to `chain_height`
/// that holds local (`Some(true)`), GUARDIAN's state (`Some(false)`) or no
/// account at all (`None`).
async fn client_ahead_of_guardian_at(
    dir: &std::path::Path,
    chain_holds: Option<bool>,
    chain_height: u32,
) -> Ahead {
    let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let config =
        MultisigGuardianConfig::new(1, vec![keystore.commitment()], Word::from([9u32, 9, 9, 9]));
    let guardian_state = MultisigGuardianBuilder::new(config)
        .with_seed([7; 32])
        .build_existing()
        .expect("multisig account builds");
    let mut local = guardian_state.clone();
    local.increment_nonce(Felt::ONE).expect("nonce increments");
    assert_ne!(local.to_commitment(), guardian_state.to_commitment());

    let on_chain = match chain_holds {
        Some(true) => vec![local.clone()],
        Some(false) => vec![guardian_state.clone()],
        None => vec![],
    };
    let mut chain = MockChainBuilder::with_accounts(on_chain)
        .expect("mock chain accepts account")
        .build()
        .expect("mock chain builds");
    if chain_height > 0 {
        chain
            .prove_until_block(chain_height)
            .expect("mock chain proves blocks");
    }
    let api = Arc::new(MockRpcApi::new(chain));
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir, api.clone(), None, keystore).await;
    client.set_node_rpc_client(api);
    client.add_or_update_account(&local, false).await.unwrap();
    client.account = Some(MultisigAccount::new(local.clone()));
    client.untracked_submission = Some(UntrackedSubmission {
        commitment: local.to_commitment(),
        submission_height: BlockNumber::from(0u32),
        expiration_block_num: BlockNumber::from(0u32),
    });

    let service = MockGuardianService::default();
    service
        .handle()
        .set_persistent_get_state(registered_state(&guardian_state));
    Ahead {
        client,
        guardian_state,
        local,
        service,
    }
}

async fn connect(client: &mut MultisigClient, service: MockGuardianService) {
    let endpoint = start_mock_server(service).await.unwrap();
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
}

/// GUARDIAN's delta at `nonce`, in `status`, recording `new_commitment`.
fn delta_at(nonce: u64, status: delta_status::Status, new_commitment: String) -> GetDeltaResponse {
    GetDeltaResponse {
        success: true,
        message: String::new(),
        delta: Some(ProtoDeltaObject {
            account_id: String::new(),
            nonce,
            prev_commitment: String::new(),
            delta_payload: String::new(),
            new_commitment,
            ack_sig: String::new(),
            candidate_at: String::new(),
            canonical_at: None,
            discarded_at: None,
            status: Some(DeltaStatus {
                status: Some(status),
                discard_reason: String::new(),
                retain_reason: String::new(),
            }),
            ack_pubkey: None,
            ack_scheme: None,
        }),
    }
}

fn at() -> String {
    "2026-01-01T00:00:00Z".to_string()
}

fn candidate_for(account: &Account) -> GetDeltaResponse {
    delta_at(
        account.nonce().as_canonical_u64(),
        delta_status::Status::CandidateAt(at()),
        word_to_hex(&account.to_commitment()),
    )
}

/// What a sync against a GUARDIAN serving one `get_delta` response did.
struct Synced {
    now: Word,
    stored: Word,
    guardian_state: Account,
    local: Account,
    asked: Vec<(String, u64)>,
}

/// Sync a client ahead of GUARDIAN against a GUARDIAN serving `delta` of
/// the local state.
async fn sync_with_delta(
    delta: impl FnOnce(&Account) -> Result<GetDeltaResponse, Status>,
) -> Synced {
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state,
        local,
        service,
    } = client_ahead_of_guardian(dir.path(), false).await;
    let service = service.with_get_delta(delta(&local));
    let requests = service.get_delta_requests_handle();
    connect(&mut client, service).await;
    client
        .sync_from_guardian()
        .await
        .expect("sync from GUARDIAN");
    let asked = requests
        .lock()
        .unwrap()
        .iter()
        .map(|request| (request.account_id.clone(), request.nonce))
        .collect();
    Synced {
        now: client.account().unwrap().commitment(),
        stored: stored_commitment(&client, &local).await,
        guardian_state,
        local,
        asked,
    }
}

fn delta_recording(local: &Account, status: delta_status::Status) -> GetDeltaResponse {
    delta_at(
        local.nonce().as_canonical_u64(),
        status,
        word_to_hex(&local.to_commitment()),
    )
}

async fn input_note_count(client: &MultisigClient) -> usize {
    client
        .miden_client
        .get_input_notes(miden_client::store::NoteFilter::All)
        .await
        .unwrap()
        .len()
}

async fn stored_commitment(client: &MultisigClient, account: &Account) -> Word {
    client
        .miden_client
        .get_account(account.id())
        .await
        .unwrap()
        .expect("account in the store")
        .to_commitment()
}

#[tokio::test]
async fn sync_rolls_back_a_stale_transaction() {
    // No expiration: the drop is miden-client's stale rule, the submission
    // still unconfirmed more than TX_DISCARD_DELTA blocks later.
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state,
        local,
        service,
    } = client_ahead_of_guardian_at(dir.path(), Some(false), 30).await;
    client.miden_client.sync_state().await.unwrap();
    let sync_height = client.miden_client.get_sync_height().await.unwrap();
    assert!(sync_height.as_u32() > crate::builder::TX_DISCARD_DELTA);
    client.untracked_submission = Some(UntrackedSubmission {
        commitment: local.to_commitment(),
        submission_height: BlockNumber::from(0u32),
        expiration_block_num: BlockNumber::from(u32::MAX),
    });
    connect(
        &mut client,
        service.with_get_delta(Err(Status::not_found("delta not found"))),
    )
    .await;

    client.sync_from_guardian().await.unwrap();

    assert_eq!(
        stored_commitment(&client, &guardian_state).await,
        guardian_state.to_commitment(),
        "the store now holds GUARDIAN's state"
    );
    assert_eq!(
        client.account().unwrap().commitment(),
        guardian_state.to_commitment()
    );
}

#[tokio::test]
async fn sync_keeps_a_transaction_still_inside_the_stale_window() {
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state: _,
        local,
        service,
    } = client_ahead_of_guardian_at(dir.path(), Some(false), 30).await;
    client.miden_client.sync_state().await.unwrap();
    let sync_height = client.miden_client.get_sync_height().await.unwrap();
    client.untracked_submission = Some(UntrackedSubmission {
        commitment: local.to_commitment(),
        submission_height: sync_height
            .checked_sub(crate::builder::TX_DISCARD_DELTA)
            .unwrap(),
        expiration_block_num: BlockNumber::from(u32::MAX),
    });
    connect(
        &mut client,
        service.with_get_delta(Err(Status::not_found("delta not found"))),
    )
    .await;

    client.sync_from_guardian().await.unwrap();

    assert_eq!(
        stored_commitment(&client, &local).await,
        local.to_commitment()
    );
}

#[tokio::test]
async fn sync_keeps_local_when_the_chain_does_not_show_guardians_state() {
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state: _,
        local,
        service,
    } = client_ahead_of_guardian_at(dir.path(), None, 0).await;
    connect(
        &mut client,
        service.with_get_delta(Err(Status::not_found("delta not found"))),
    )
    .await;

    client.sync_from_guardian().await.unwrap();

    assert_eq!(
        stored_commitment(&client, &local).await,
        local.to_commitment(),
        "no chain evidence for GUARDIAN's state keeps local"
    );
}

#[tokio::test]
async fn sync_rolls_back_a_dropped_transaction_guardian_no_longer_holds() {
    let synced = sync_with_delta(|_| Err(Status::not_found("delta not found"))).await;
    assert_eq!(
        synced.now,
        synced.guardian_state.to_commitment(),
        "the device returns to GUARDIAN's state, which the chain holds"
    );
    assert_eq!(synced.stored, synced.guardian_state.to_commitment());
    assert_eq!(
        synced.asked,
        vec![(
            synced.local.id().to_string(),
            synced.local.nonce().as_canonical_u64()
        )],
        "GUARDIAN is asked about the local state's own nonce"
    );
}

#[tokio::test]
async fn sync_keeps_a_transaction_the_node_may_still_include() {
    // GUARDIAN can forget a delta whose transaction is still pending (an
    // expired retained row, retention disabled, a failed best-effort switch
    // push), so its absence is no evidence the transaction will not land.
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state: _,
        local,
        service,
    } = client_ahead_of_guardian(dir.path(), false).await;
    let sync_height = client.miden_client.get_sync_height().await.unwrap();
    client.untracked_submission = Some(UntrackedSubmission {
        commitment: local.to_commitment(),
        submission_height: sync_height,
        expiration_block_num: BlockNumber::from(u32::MAX),
    });
    connect(
        &mut client,
        service.with_get_delta(Err(Status::not_found("delta not found"))),
    )
    .await;

    client.sync_from_guardian().await.unwrap();

    assert_eq!(
        client.account().unwrap().commitment(),
        local.to_commitment(),
        "a transaction miden-client would still count as pending is not rolled back"
    );
    assert_eq!(
        stored_commitment(&client, &local).await,
        local.to_commitment()
    );
}

#[tokio::test]
async fn sync_keeps_a_state_this_client_did_not_submit() {
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state: _,
        local,
        service,
    } = client_ahead_of_guardian(dir.path(), false).await;
    client.untracked_submission = None;
    connect(
        &mut client,
        service.with_get_delta(Err(Status::not_found("delta not found"))),
    )
    .await;

    client.sync_from_guardian().await.unwrap();

    assert_eq!(
        client.account().unwrap().commitment(),
        local.to_commitment(),
        "without a submission of its own there is no node evidence"
    );
}

#[tokio::test]
async fn sync_keeps_a_transaction_guardian_still_holds_as_its_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state: _,
        local,
        service,
    } = client_ahead_of_guardian(dir.path(), false).await;
    let service = service.with_get_delta(Ok(candidate_for(&local)));
    connect(&mut client, service).await;

    client
        .sync_from_guardian()
        .await
        .expect("sync from GUARDIAN keeps local");

    assert_eq!(
        client.account().unwrap().commitment(),
        local.to_commitment(),
        "a transaction that may still land is not rolled back"
    );
}

#[tokio::test]
async fn sync_keeps_a_local_state_the_chain_holds() {
    // The transaction landed and GUARDIAN has not canonicalized it yet: the
    // usual reason local is ahead.
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state: _,
        local,
        service,
    } = client_ahead_of_guardian(dir.path(), true).await;
    let service = service.with_get_delta(Err(Status::not_found("delta not found")));
    connect(&mut client, service).await;

    client
        .sync_from_guardian()
        .await
        .expect("sync from GUARDIAN keeps local");

    assert_eq!(
        client.account().unwrap().commitment(),
        local.to_commitment()
    );
}

#[tokio::test]
async fn sync_keeps_local_when_guardian_cannot_say() {
    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state: _,
        local,
        service,
    } = client_ahead_of_guardian(dir.path(), false).await;
    let service = service.with_get_delta(Err(Status::unavailable("guardian down")));
    connect(&mut client, service).await;

    client
        .sync_from_guardian()
        .await
        .expect("sync from GUARDIAN keeps local");

    assert_eq!(
        client.account().unwrap().commitment(),
        local.to_commitment(),
        "an unreadable candidate status is no evidence"
    );
}

#[tokio::test]
async fn sync_keeps_a_retained_delta_guardian_may_still_promote() {
    let synced = sync_with_delta(|local| {
        Ok(delta_recording(
            local,
            delta_status::Status::RetainedAt(at()),
        ))
    })
    .await;
    assert_eq!(
        synced.now,
        synced.local.to_commitment(),
        "a retained delta may still land"
    );
}

#[tokio::test]
async fn sync_rolls_back_a_discarded_delta() {
    let synced = sync_with_delta(|local| {
        Ok(delta_recording(
            local,
            delta_status::Status::DiscardedAt(at()),
        ))
    })
    .await;
    assert_eq!(synced.now, synced.guardian_state.to_commitment());
    assert_eq!(synced.stored, synced.guardian_state.to_commitment());
}

#[tokio::test]
async fn sync_rolls_back_past_a_delta_recording_another_state() {
    let synced = sync_with_delta(|local| {
        Ok(delta_at(
            local.nonce().as_canonical_u64(),
            delta_status::Status::RetainedAt(at()),
            word_to_hex(&Word::from([4u32, 3, 2, 1])),
        ))
    })
    .await;
    assert_eq!(
        synced.now,
        synced.guardian_state.to_commitment(),
        "another device's delta at that nonce does not keep this one alive"
    );
}

#[tokio::test]
async fn sync_keeps_local_when_the_delta_commitment_does_not_parse() {
    let synced = sync_with_delta(|local| {
        Ok(delta_at(
            local.nonce().as_canonical_u64(),
            delta_status::Status::CanonicalAt(at()),
            "not a commitment".to_string(),
        ))
    })
    .await;
    assert_eq!(
        synced.now,
        synced.local.to_commitment(),
        "an unreadable delta may be the local one"
    );
}

#[test]
fn a_submission_is_dropped_as_miden_client_discards_its_own() {
    let submission = UntrackedSubmission {
        commitment: Word::default(),
        submission_height: BlockNumber::from(100u32),
        expiration_block_num: BlockNumber::from(150u32),
    };
    assert!(!submission.dropped_at(BlockNumber::from(120u32)));
    assert!(
        submission.dropped_at(BlockNumber::from(121u32)),
        "stale once confirmed blocks pass TX_DISCARD_DELTA"
    );

    let long_lived = UntrackedSubmission {
        expiration_block_num: BlockNumber::from(u32::MAX),
        ..submission
    };
    assert!(!long_lived.dropped_at(BlockNumber::from(120u32)));
    let expiring = UntrackedSubmission {
        expiration_block_num: BlockNumber::from(110u32),
        ..submission
    };
    assert!(!expiring.dropped_at(BlockNumber::from(109u32)));
    assert!(
        expiring.dropped_at(BlockNumber::from(110u32)),
        "expired at its expiration block"
    );
}

#[tokio::test]
async fn sync_rollback_carries_an_imported_private_note_over() {
    // The rollback starts from a fresh store; an input note the user
    // imported into this session must survive it.
    use miden_client::note::{NoteFile, NoteSyncHint};
    use miden_protocol::block::BlockNumber;
    use miden_protocol::note::NoteType;

    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state,
        service,
        ..
    } = client_ahead_of_guardian(dir.path(), false).await;
    let note = super::test_support::p2id_note_for(&guardian_state, 4, NoteType::Private);
    let tag = note.metadata().tag();
    let file = NoteFile::ExpectedNote {
        details: note.into(),
        sync_hint: NoteSyncHint::new(BlockNumber::from(0u32), tag),
    };
    client
        .import_note_from_bytes(&file.to_bytes())
        .await
        .unwrap();
    assert_eq!(input_note_count(&client).await, 1);
    connect(
        &mut client,
        service.with_get_delta(Err(Status::not_found("delta not found"))),
    )
    .await;

    client.sync_from_guardian().await.unwrap();

    assert_eq!(
        client.account().unwrap().commitment(),
        guardian_state.to_commitment()
    );
    assert_eq!(
        input_note_count(&client).await,
        1,
        "rolling back the account must keep the imported note"
    );
}

#[tokio::test]
async fn a_failed_rollback_keeps_the_current_store_and_its_notes() {
    // The fresh store is installed only once it holds the account and every
    // carried note: a note the fresh store cannot import leaves the current
    // store, notes included, for the next sync to retry.
    use miden_client::note::{NoteFile, NoteSyncHint};
    use miden_protocol::block::BlockNumber;
    use miden_protocol::note::NoteType;

    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state,
        local,
        ..
    } = client_ahead_of_guardian(dir.path(), false).await;
    let note = super::test_support::p2id_note_for(&guardian_state, 5, NoteType::Private);
    let tag = note.metadata().tag();
    let unknown_to_the_node = NoteFile::NoteId(note.id());
    client
        .import_note_from_bytes(
            &NoteFile::ExpectedNote {
                details: note.into(),
                sync_hint: NoteSyncHint::new(BlockNumber::from(0u32), tag),
            }
            .to_bytes(),
        )
        .await
        .unwrap();

    client
        .roll_back_store(&guardian_state, &[unknown_to_the_node])
        .await
        .expect_err("the node does not know the note, so the fresh store cannot import it");

    assert_eq!(
        stored_commitment(&client, &local).await,
        local.to_commitment(),
        "the current store still holds the local state"
    );
    assert_eq!(input_note_count(&client).await, 1, "and its notes");
}

#[tokio::test]
async fn a_note_created_before_a_rollback_still_exports() {
    // A fresh store cannot take over output-note records, so the export of a
    // note this session created reads it from the store the rollback replaced.
    use miden_client::account::{AccountBuilder, AccountBuilderSchemaCommitmentExt, AccountType};
    use miden_client::auth::AuthSchemeId;
    use miden_client::keystore::Keystore;
    use miden_client::testing::common::{auth_component, fungible_faucet_component};
    use miden_client::transaction::TransactionRequestBuilder;
    use miden_protocol::asset::FungibleAsset;
    use miden_protocol::note::NoteType;

    let dir = tempfile::tempdir().unwrap();
    let Ahead {
        mut client,
        guardian_state,
        service,
        ..
    } = client_ahead_of_guardian(dir.path(), false).await;

    let (auth, key) = auth_component(AuthSchemeId::Falcon512Poseidon2).unwrap();
    let (faucet_component, policies) = fungible_faucet_component().unwrap();
    let faucet = AccountBuilder::new([3; 32])
        .account_type(AccountType::Private)
        .with_component(auth)
        .with_component(faucet_component)
        .with_component(miden_client::account::component::BasicWallet)
        .with_components(policies)
        .build_with_schema_commitment()
        .unwrap();
    client
        .miden_client
        .authenticator()
        .unwrap()
        .add_key(&key, faucet.id())
        .await
        .unwrap();
    client
        .miden_client
        .add_account(&faucet, false)
        .await
        .unwrap();
    client.miden_client.sync_state().await.unwrap();
    let mint = TransactionRequestBuilder::new()
        .build_mint_fungible_asset(
            FungibleAsset::new(faucet.id(), 10).unwrap(),
            guardian_state.id(),
            NoteType::Private,
            client.miden_client.rng(),
        )
        .unwrap();
    let note_id = mint.expected_output_own_notes()[0].id().to_hex();
    let executed = client
        .miden_client
        .execute_transaction(faucet.id(), mint)
        .await
        .unwrap();
    let height = client.miden_client.get_sync_height().await.unwrap();
    let update = client
        .miden_client
        .get_transaction_store_update(&executed, height)
        .await
        .unwrap();
    client
        .miden_client
        .apply_transaction_update(update)
        .await
        .unwrap();
    client.export_note_to_bytes(&note_id).await.unwrap();

    connect(
        &mut client,
        service.with_get_delta(Err(Status::not_found("delta not found"))),
    )
    .await;
    client.sync_from_guardian().await.unwrap();
    assert_eq!(
        client.account().unwrap().commitment(),
        guardian_state.to_commitment(),
        "the rollback ran"
    );

    client
        .export_note_to_bytes(&note_id)
        .await
        .expect("the note is still exportable after the rollback");
}
