//! Cross-height binding regression tests (issue #409), driven against the
//! in-process mock GUARDIAN gRPC server and a mock chain.
//!
//! Since protocol 0.16 the signed transaction summary binds the reference
//! block commitment, so a summary can only be reproduced by an execution at
//! the block it was built at. Summary-binding verification therefore
//! re-executes at the proposal's chain anchor rather than at the verifier's
//! own sync height. Before chain anchors it re-executed at the tip, which
//! made a second signer who synced at a later block than the proposer fail
//! with "metadata does not match tx_summary" and abort the whole listing.

use std::sync::Arc;

use guardian_client::GetDeltaProposalsResponse;
use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use miden_protocol::Word;
use miden_protocol::note::NoteType;
use miden_protocol::transaction::RawOutputNote;

use super::test_support::{
    chain_with_notes, multisig_account, offline_client_parts_with_keystore,
    offline_client_with_node_parts, p2id_note_for, pending_proto_delta, registered_state,
};
use crate::account::MultisigAccount;
use crate::execution::build_final_transaction_request;
use crate::keystore::{GuardianKeyStore, KeyManager};
use crate::payload::ProposalPayload;
use crate::proposal::{SerializedNote, TransactionType};
use crate::transaction::{chain_anchor_to_base64, execute_for_summary, word_to_hex};

/// Issue #409: a cosigner on a fresh store, syncing at a later block than
/// the proposer, lists a pending proposal through the strict (signing-path)
/// listing. Binding verification reproduces the signed summary because it
/// re-executes at the proposal's anchor; re-executing at the cosigner's own
/// tip — what verification did before anchors — provably does not.
#[tokio::test]
async fn cosigner_at_a_later_sync_height_verifies_a_pending_proposal_at_its_anchor() {
    // The proposer, whose key is the account's one cosigner.
    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let guardian_commitment = Word::from([9u32, 9, 9, 9]);
    let account = multisig_account(signer_commitment, guardian_commitment, 47);

    // A private P2ID note for the account, committed on the mock chain. The
    // proposal embeds its bytes, so every verifier rebuilds the request from
    // the proposal alone (consume-notes v2).
    let note = p2id_note_for(&account, 7, NoteType::Private);
    let api = chain_with_notes(vec![RawOutputNote::Full(note.clone())]);

    let dir1 = tempfile::tempdir().unwrap();
    let (mut proposer, _store1) =
        offline_client_parts_with_keystore(dir1.path(), api.clone(), None, keystore.clone()).await;
    proposer.set_node_rpc_client(api.clone());
    proposer
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    proposer.account = Some(MultisigAccount::new(account.clone()));
    proposer.miden_client.sync_state().await.unwrap();

    // Proposal creation authenticates the notes before capturing the anchor
    // (see `ProposalBuilder::build_consume_notes`); mirror it here.
    let node_rpc = proposer.node_rpc_client();
    crate::transaction::ensure_notes_authenticated(
        &mut proposer.miden_client,
        &node_rpc,
        std::slice::from_ref(&note),
    )
    .await
    .unwrap();

    let salt = Word::from([5u32, 6, 7, 8]);
    let tx_type =
        TransactionType::consume_notes_v2(vec![note.id()], vec![SerializedNote::from_note(&note)]);
    let tx_request = build_final_transaction_request(
        &proposer.miden_client,
        &tx_type,
        &account,
        salt,
        Vec::new(),
        None,
        Some(&[]),
        proposer.key_manager.scheme(),
    )
    .await
    .unwrap();
    let (tx_summary, chain_anchor) =
        execute_for_summary(&mut proposer.miden_client, account.id(), tx_request)
            .await
            .unwrap();
    let proposal_id = word_to_hex(&tx_summary.to_commitment());
    let anchor_height = chain_anchor.block_num();

    let delta_payload = ProposalPayload::new(&tx_summary)
        .with_note_consumption_metadata_v2(
            vec![note.id().to_hex()],
            vec![SerializedNote::from_note(&note).into_inner()],
            word_to_hex(&salt),
        )
        .with_required_signatures(1)
        .with_chain_anchor(chain_anchor_to_base64(&chain_anchor))
        .to_json()
        .to_string();

    // Mock GUARDIAN serving the registered state and the pending proposal.
    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    handle.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![pending_proto_delta(
            &account,
            1,
            delta_payload,
            &word_to_hex(&signer_commitment),
        )],
    });

    // The chain moves on before the second signer shows up.
    api.advance_blocks(5);

    // The second signer: fresh store, has never seen this account, syncs at
    // the new tip — strictly past the block the proposal was anchored at.
    let dir2 = tempfile::tempdir().unwrap();
    let (mut cosigner, _store2) = offline_client_with_node_parts(dir2.path(), api.clone()).await;
    cosigner
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    cosigner
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    cosigner.account = Some(MultisigAccount::new(account.clone()));
    cosigner.miden_client.sync_state().await.unwrap();
    let cosigner_height = cosigner.miden_client.get_sync_height().await.unwrap();
    assert!(
        cosigner_height > anchor_height,
        "the cosigner must sync past the anchor ({anchor_height}) to exercise the bug; \
         synced at {cosigner_height}"
    );

    // The strict listing verifies every proposal's binding — the very call
    // that failed for the second signer in #409.
    let proposals = cosigner
        .list_proposals()
        .await
        .expect("the anchored re-execution reproduces the signed summary at any sync height");
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    assert!(
        proposals[0].id.eq_ignore_ascii_case(&proposal_id),
        "listed {} but the proposer signed {proposal_id}",
        proposals[0].id
    );

    // Control: the same request re-executed at the cosigner's own tip yields
    // a different commitment, so this test would fail were verification to
    // fall back to the sync height.
    let tip_request = build_final_transaction_request(
        &cosigner.miden_client,
        &tx_type,
        &account,
        salt,
        Vec::new(),
        None,
        Some(&[]),
        cosigner.key_manager.scheme(),
    )
    .await
    .unwrap();
    let (tip_summary, tip_anchor) =
        execute_for_summary(&mut cosigner.miden_client, account.id(), tip_request)
            .await
            .unwrap();
    assert!(tip_anchor.block_num() > anchor_height);
    assert_ne!(
        word_to_hex(&tip_summary.to_commitment()),
        proposal_id,
        "a tip re-execution must not reproduce a summary anchored at an earlier block"
    );
}

/// The live shape of issue #409 on an anchored client: the proposer's store
/// held the public notes with their inclusion proofs (synced), so its signed
/// summary commits to *authenticated* consumption; a cosigner on a fresh
/// store holds nothing and, left to miden-client alone, would rebuild an
/// *unauthenticated* consumption whose summary differs. The rebuild now
/// authenticates the notes first, so the strict listing reproduces the
/// signed commitment and leaves the cosigner's store authenticated too.
#[tokio::test]
async fn fresh_cosigner_verifies_a_consume_proposal_whose_proposer_held_the_notes_with_proofs() {
    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let guardian_commitment = Word::from([9u32, 9, 9, 9]);
    let account = multisig_account(signer_commitment, guardian_commitment, 48);

    // Two PUBLIC notes committed on the mock chain.
    let notes = vec![
        p2id_note_for(&account, 11, NoteType::Public),
        p2id_note_for(&account, 12, NoteType::Public),
    ];
    let api = chain_with_notes(notes.iter().cloned().map(RawOutputNote::Full).collect());

    let dir1 = tempfile::tempdir().unwrap();
    let (mut proposer, _store1) =
        offline_client_parts_with_keystore(dir1.path(), api.clone(), None, keystore.clone()).await;
    proposer.set_node_rpc_client(api.clone());
    proposer
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    proposer.account = Some(MultisigAccount::new(account.clone()));
    proposer.miden_client.sync_state().await.unwrap();
    // The proposer holds the notes the way a synced wallet does: fetched from
    // the node with their inclusion proofs, i.e. authenticated.
    proposer
        .miden_client
        .import_notes(
            &notes
                .iter()
                .map(|n| miden_client::note::NoteFile::NoteId(n.id()))
                .collect::<Vec<_>>(),
        )
        .await
        .unwrap();
    for note in &notes {
        let record = proposer
            .miden_client
            .get_input_note(note.id())
            .await
            .unwrap()
            .expect("the proposer tracks the public note");
        assert!(
            record.is_authenticated(),
            "the proposer's note must carry its inclusion proof"
        );
    }

    let salt = Word::from([5u32, 6, 7, 8]);
    let tx_type = TransactionType::consume_notes_v2(
        notes.iter().map(miden_protocol::note::Note::id).collect(),
        notes.iter().map(SerializedNote::from_note).collect(),
    );
    let tx_request = build_final_transaction_request(
        &proposer.miden_client,
        &tx_type,
        &account,
        salt,
        Vec::new(),
        None,
        Some(&[]),
        proposer.key_manager.scheme(),
    )
    .await
    .unwrap();
    let (tx_summary, chain_anchor) =
        execute_for_summary(&mut proposer.miden_client, account.id(), tx_request)
            .await
            .unwrap();
    let proposal_id = word_to_hex(&tx_summary.to_commitment());

    let delta_payload = ProposalPayload::new(&tx_summary)
        .with_note_consumption_metadata_v2(
            notes.iter().map(|n| n.id().to_hex()).collect(),
            notes
                .iter()
                .map(|n| SerializedNote::from_note(n).into_inner())
                .collect(),
            word_to_hex(&salt),
        )
        .with_required_signatures(1)
        .with_chain_anchor(chain_anchor_to_base64(&chain_anchor))
        .to_json()
        .to_string();

    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    handle.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![pending_proto_delta(
            &account,
            1,
            delta_payload,
            &word_to_hex(&signer_commitment),
        )],
    });

    api.advance_blocks(3);

    // The cosigner: fresh store, never synced these notes (they predate its
    // tracking of the account), so it holds no record at all.
    let dir2 = tempfile::tempdir().unwrap();
    let (mut cosigner, _store2) = offline_client_with_node_parts(dir2.path(), api.clone()).await;
    cosigner
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    cosigner
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    cosigner.account = Some(MultisigAccount::new(account.clone()));
    cosigner.miden_client.sync_state().await.unwrap();
    for note in &notes {
        let record = cosigner
            .miden_client
            .get_input_note(note.id())
            .await
            .unwrap();
        assert!(
            record.is_none_or(|r| !r.is_authenticated()),
            "precondition: the cosigner must not already hold the note authenticated"
        );
    }

    let proposals = cosigner
        .list_proposals()
        .await
        .expect("the rebuild authenticates the notes first, so the summary reproduces");
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    assert!(proposals[0].id.eq_ignore_ascii_case(&proposal_id));

    for note in &notes {
        let record = cosigner
            .miden_client
            .get_input_note(note.id())
            .await
            .unwrap()
            .expect("verification imported the note");
        assert!(
            record.is_authenticated(),
            "verification must leave the note authenticated, ready for execution"
        );
    }
}

/// `ensure_notes_authenticated` imports a note the store has never seen with
/// the node's inclusion proof, is a no-op (no node round trip) once the note
/// is authenticated, and refuses a note the chain does not know, naming it.
#[tokio::test]
async fn ensure_notes_authenticated_imports_committed_notes_and_refuses_uncommitted_ones() {
    let keystore = Arc::new(GuardianKeyStore::generate());
    let account = multisig_account(keystore.commitment(), Word::from([9u32, 9, 9, 9]), 49);
    let committed = p2id_note_for(&account, 21, NoteType::Private);
    let uncommitted = p2id_note_for(&account, 22, NoteType::Private);
    let api = chain_with_notes(vec![RawOutputNote::Full(committed.clone())]);

    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api.clone(), None, keystore).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, true).await.unwrap();
    client.miden_client.sync_state().await.unwrap();
    assert!(
        client
            .miden_client
            .get_input_note(committed.id())
            .await
            .unwrap()
            .is_none(),
        "precondition: a private note is not discovered by syncing"
    );

    let node_rpc = client.node_rpc_client();
    crate::transaction::ensure_notes_authenticated(
        &mut client.miden_client,
        &node_rpc,
        std::slice::from_ref(&committed),
    )
    .await
    .expect("a committed note is authenticated from the node's proof");
    let record = client
        .miden_client
        .get_input_note(committed.id())
        .await
        .unwrap()
        .expect("the note was imported");
    assert!(
        record.is_authenticated(),
        "imported with its inclusion proof"
    );

    // Idempotent: nothing left to do, no node round trip.
    let calls_before = api.get_notes_by_id_call_count();
    crate::transaction::ensure_notes_authenticated(
        &mut client.miden_client,
        &node_rpc,
        std::slice::from_ref(&committed),
    )
    .await
    .unwrap();
    assert_eq!(api.get_notes_by_id_call_count(), calls_before);

    let err = crate::transaction::ensure_notes_authenticated(
        &mut client.miden_client,
        &node_rpc,
        std::slice::from_ref(&uncommitted),
    )
    .await
    .expect_err("a note the chain does not know cannot be authenticated");
    match err {
        crate::error::MultisigError::ConsumeNoteNotAuthenticated { note_id, reason } => {
            assert_eq!(note_id, uncommitted.id());
            assert!(reason.contains("not committed"), "reason: {reason}");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}
