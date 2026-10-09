//! Cross-height binding regression tests (issues #409 and #462), driven
//! against the in-process mock GUARDIAN gRPC server and a mock chain.
//!
//! Since protocol 0.17 a multisig summary binds the block its auth args name
//! (the bound block), not the block the transaction executes against. The
//! request declares that block, so every party reproduces the summary at its
//! own chain tip. Anchored re-execution, which the 0.16 line needed, loaded
//! foreign accounts (the fee faucet on every fee-paying transaction) at the
//! bound block, and a node serves that state only ~50 blocks back (#462).

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
use crate::transaction::{
    deserialize_transaction_request, execute_for_summary_at_tip, word_to_hex,
};

/// Issue #409: a cosigner on a fresh store, syncing at a later block than
/// the proposer, lists a pending proposal through the strict (signing-path)
/// listing. Binding verification reproduces the signed summary at the
/// cosigner's own tip because the rebuilt request binds, and declares, the
/// block the proposer bound; a request bound to the cosigner's own sync
/// height provably does not reproduce it.
#[tokio::test]
async fn cosigner_at_a_later_sync_height_verifies_a_pending_proposal_at_its_tip() {
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

    // Proposal creation authenticates the notes before deriving the summary
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
    let auth_args = proposer.multisig_auth_args(salt, None, None).await.unwrap();
    let tx_request = build_final_transaction_request(
        &proposer.miden_client,
        &tx_type,
        &account,
        &auth_args,
        Vec::new(),
        None,
        Some(&[]),
        proposer.key_manager.scheme(),
    )
    .await
    .unwrap();
    let tx_summary =
        execute_for_summary_at_tip(&mut proposer.miden_client, account.id(), tx_request)
            .await
            .unwrap();
    let proposal_id = word_to_hex(&tx_summary.to_commitment());
    let bound_height = tx_summary.block_number();

    let delta_payload = ProposalPayload::new(&tx_summary)
        .with_note_consumption_metadata_v2(
            vec![note.id().to_hex()],
            vec![SerializedNote::from_note(&note).into_inner()],
            word_to_hex(&salt),
        )
        .with_required_signatures(1)
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
    // the new tip, strictly past the block the proposal binds.
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
        cosigner_height > bound_height,
        "the cosigner must sync past the bound block ({bound_height}) to exercise the bug; \
         synced at {cosigner_height}"
    );

    // The strict listing verifies every proposal's binding — the very call
    // that failed for the second signer in #409.
    let proposals = cosigner
        .list_proposals()
        .await
        .expect("the tip re-execution reproduces the signed summary at any later sync height");
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    assert!(
        proposals[0].id.eq_ignore_ascii_case(&proposal_id),
        "listed {} but the proposer signed {proposal_id}",
        proposals[0].id
    );
    assert!(
        proposals[0].is_verified(),
        "the listing must have reproduced the signed summary at the tip: {:?}",
        proposals[0].verification
    );

    // Control: the same transaction bound to the cosigner's own sync height
    // yields a different commitment, so the listing reproduced the summary
    // because it rebuilt at the bound block, not because every block agrees.
    let auth_args = cosigner.multisig_auth_args(salt, None, None).await.unwrap();
    let tip_request = build_final_transaction_request(
        &cosigner.miden_client,
        &tx_type,
        &account,
        &auth_args,
        Vec::new(),
        None,
        Some(&[]),
        cosigner.key_manager.scheme(),
    )
    .await
    .unwrap();
    let tip_summary =
        execute_for_summary_at_tip(&mut cosigner.miden_client, account.id(), tip_request)
            .await
            .unwrap();
    assert!(tip_summary.block_number() > bound_height);
    assert_ne!(
        word_to_hex(&tip_summary.to_commitment()),
        proposal_id,
        "a request bound to a later block must not reproduce the proposer's summary"
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
    let auth_args = proposer.multisig_auth_args(salt, None, None).await.unwrap();
    let tx_request = build_final_transaction_request(
        &proposer.miden_client,
        &tx_type,
        &account,
        &auth_args,
        Vec::new(),
        None,
        Some(&[]),
        proposer.key_manager.scheme(),
    )
    .await
    .unwrap();
    let tx_summary =
        execute_for_summary_at_tip(&mut proposer.miden_client, account.id(), tx_request)
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
    assert!(
        proposals[0].is_verified(),
        "the listing must have reproduced the signed summary with authenticated notes: {:?}",
        proposals[0].verification
    );

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

/// Issue #462: one proposal whose summary binding cannot be verified must
/// not hide the others. The strict listing returns it with
/// `verification_error` set and the healthy proposal untouched; signing the
/// unverifiable one still fails, because signing re-verifies. A signer
/// update is used so the case does not depend on any input-note handling.
#[tokio::test]
async fn listing_reports_an_unverifiable_proposal_instead_of_failing_the_whole_listing() {
    use crate::transaction::build_update_signers_transaction_request;

    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let guardian_commitment = Word::from([9u32, 9, 9, 9]);
    let account = multisig_account(signer_commitment, guardian_commitment, 50);
    let api = chain_with_notes(Vec::new());

    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api.clone(), None, keystore.clone()).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, true).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    // An add-cosigner proposal: the same signer set plus one, threshold kept.
    let new_cosigner = Word::from([7u32, 7, 7, 7]);
    let signers = vec![signer_commitment, new_cosigner];
    let signers_hex: Vec<String> = signers.iter().map(word_to_hex).collect();
    let salt = Word::from([5u32, 6, 7, 8]);
    let auth_args = client.multisig_auth_args(salt, None, None).await.unwrap();
    let (tx_request, _) = build_update_signers_transaction_request(
        1,
        &signers,
        &auth_args,
        std::iter::empty(),
        client.key_manager.scheme(),
    )
    .unwrap();
    let tx_summary = execute_for_summary_at_tip(&mut client.miden_client, account.id(), tx_request)
        .await
        .unwrap();
    let good_id = word_to_hex(&tx_summary.to_commitment());
    let payload = |salt_hex: String| {
        ProposalPayload::new(&tx_summary)
            .with_add_signer_metadata(1, signers_hex.clone(), salt_hex)
            .with_required_signatures(1)
            .to_json()
            .to_string()
    };
    // The healthy proposal and a copy whose served salt is wrong. Since 0.17
    // the summary binds the salt itself, so the mismatch is caught by name
    // before any rebuild. Same summary bytes, so the same id — GUARDIAN never
    // serves that, so give it a distinct nonce to keep the two apart in the
    // listing.
    let good = pending_proto_delta(
        &account,
        1,
        payload(word_to_hex(&salt)),
        &word_to_hex(&signer_commitment),
    );
    let bad = pending_proto_delta(
        &account,
        2,
        payload(word_to_hex(&Word::from([1u32, 1, 1, 1]))),
        &word_to_hex(&signer_commitment),
    );

    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    handle.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![bad, good],
    });
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    let proposals = client
        .list_proposals()
        .await
        .expect("an unverifiable proposal is reported, not fatal");
    assert_eq!(proposals.len(), 2, "proposals: {proposals:?}");
    let (unverifiable, verified): (Vec<_>, Vec<_>) =
        proposals.iter().partition(|p| !p.is_verified());
    assert_eq!(verified.len(), 1, "proposals: {proposals:?}");
    assert!(verified[0].id.eq_ignore_ascii_case(&good_id));
    assert_eq!(unverifiable.len(), 1);
    assert_eq!(unverifiable[0].nonce, 2);
    match &unverifiable[0].verification {
        crate::proposal::ProposalVerification::Failed { retryable, message } => {
            assert!(
                !retryable,
                "a tampered proposal is not worth retrying: {message}"
            );
            assert!(
                message.contains("metadata salt does not match the salt bound into its tx_summary"),
                "message: {message}"
            );
        }
        other => panic!("expected a failed verification, got {other:?}"),
    }
    assert!(!unverifiable[0].is_actionable());
}

/// Issue #462 review: the proposal `sign_proposal` returns is parsed from
/// GUARDIAN's fresh response, so it must be re-verified there, or it comes
/// back `Unchecked` and is never actionable even after the final signature.
#[tokio::test]
async fn sign_proposal_returns_a_verified_actionable_proposal_after_the_final_signature() {
    use crate::transaction::build_update_signers_transaction_request;
    use guardian_client::{GetDeltaProposalResponse, SignDeltaProposalResponse};

    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let signer_hex = word_to_hex(&signer_commitment);
    let account = multisig_account(signer_commitment, Word::from([9u32, 9, 9, 9]), 51);
    let api = chain_with_notes(Vec::new());

    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api.clone(), None, keystore.clone()).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, true).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    // A 1-of-1 add-cosigner proposal, served unsigned; the signer's own
    // signature is the final one.
    let new_cosigner = Word::from([7u32, 7, 7, 7]);
    let signers = vec![signer_commitment, new_cosigner];
    let signers_hex: Vec<String> = signers.iter().map(word_to_hex).collect();
    let salt = Word::from([5u32, 6, 7, 8]);
    let auth_args = client.multisig_auth_args(salt, None, None).await.unwrap();
    let (tx_request, _) = build_update_signers_transaction_request(
        1,
        &signers,
        &auth_args,
        std::iter::empty(),
        client.key_manager.scheme(),
    )
    .unwrap();
    let tx_summary = execute_for_summary_at_tip(&mut client.miden_client, account.id(), tx_request)
        .await
        .unwrap();
    let proposal_id = word_to_hex(&tx_summary.to_commitment());
    let payload = ProposalPayload::new(&tx_summary)
        .with_add_signer_metadata(1, signers_hex.clone(), word_to_hex(&salt))
        .with_required_signatures(1);
    let unsigned = pending_proto_delta(&account, 1, payload.to_json().to_string(), &signer_hex);

    // What GUARDIAN hands back after accepting the signature: the same
    // payload carrying it, which meets the 1-of-1 threshold.
    let mut signed_payload = payload;
    signed_payload
        .signatures
        .push(guardian_shared::DeltaSignature {
            signer_id: signer_hex.clone(),
            signature: guardian_shared::ProposalSignature::Falcon {
                signature: client.key_manager.sign_word_hex(tx_summary.to_commitment()),
            },
        });
    let signed = pending_proto_delta(
        &account,
        1,
        signed_payload.to_json().to_string(),
        &signer_hex,
    );

    let service =
        MockGuardianService::default().with_sign_delta_proposal(Ok(SignDeltaProposalResponse {
            success: true,
            message: String::new(),
            delta: Some(signed),
        }));
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    handle.set_persistent_get_delta_proposal(GetDeltaProposalResponse {
        success: true,
        message: String::new(),
        proposal: Some(unsigned),
    });
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    let updated = client.sign_proposal(&proposal_id).await.unwrap();
    assert!(
        updated.is_verified(),
        "the signed response must be re-verified, got {:?}",
        updated.verification
    );
    assert!(
        updated.status.is_ready(),
        "1-of-1 threshold met: {:?}",
        updated.status
    );
    assert!(updated.is_actionable());
}

/// The reported shape of issue #538: a producer builds a custom request at
/// block N, the chain moves on, and the proposer syncs past N before it
/// proposes. The proposal is created, cosigned, acknowledged by GUARDIAN with
/// a real key, and lands on chain. 0.18.0 refused it at creation with
/// `SummaryAnchorMismatch`: an anchor can only be captured at the proposer's
/// sync height, and the summary binds N.
#[tokio::test]
async fn custom_proposal_bound_to_an_older_block_lands_after_the_proposer_syncs_past_it() {
    use guardian_client::{
        DeltaObject as ProtoDeltaObject, GetDeltaProposalResponse, PushDeltaProposalResponse,
        PushDeltaResponse, SignDeltaProposalResponse,
    };
    use guardian_shared::{DeltaSignature, ProposalSignature, SignatureScheme};
    use miden_client::rpc::encryption::TransactionEncryptionKey;
    use miden_client::store::TransactionFilter;
    use miden_client::transaction::TransactionStatus;
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };
    use miden_protocol::crypto::dsa::eddsa_25519_sha512::SigningKey;
    use miden_protocol::utils::serde::Serializable;

    use crate::procedures::ProcedureName;

    // 2-of-2, with a real GUARDIAN key so its acknowledgement verifies in the
    // auth procedure.
    let proposer_keys = Arc::new(GuardianKeyStore::generate());
    let cosigner_keys = Arc::new(GuardianKeyStore::generate());
    let guardian_keys = GuardianKeyStore::generate();
    let account = MultisigGuardianBuilder::new(
        MultisigGuardianConfig::new(
            2,
            vec![proposer_keys.commitment(), cosigner_keys.commitment()],
            guardian_keys.commitment(),
        )
        .with_signature_scheme(SignatureScheme::Falcon),
    )
    .with_seed([54; 32])
    .build()
    .expect("2-of-2 multisig account builds");
    let api = chain_with_notes(Vec::new());

    let proposer_dir = tempfile::tempdir().unwrap();
    let (mut proposer, _proposer_store) = offline_client_parts_with_keystore(
        proposer_dir.path(),
        api.clone(),
        None,
        proposer_keys.clone(),
    )
    .await;
    proposer.set_node_rpc_client(api.clone());
    proposer
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    proposer.account = Some(MultisigAccount::new(account.clone()));
    proposer.miden_client.sync_state().await.unwrap();

    // The producer builds at the proposer's sync height N.
    let bound_block = proposer.miden_client.get_sync_height().await.unwrap();
    let auth_args = proposer
        .multisig_auth_args(Word::from([5u32, 6, 7, 8]), None, None)
        .await
        .unwrap();
    let tx_type = TransactionType::UpdateProcedureThreshold {
        procedure: ProcedureName::SendAsset,
        new_threshold: 1,
    };
    let tx_request = build_final_transaction_request(
        &proposer.miden_client,
        &tx_type,
        &account,
        &auth_args,
        Vec::new(),
        None,
        None,
        proposer.key_manager.scheme(),
    )
    .await
    .unwrap();
    let request_bytes = tx_request.to_bytes();
    let expected_summary =
        execute_for_summary_at_tip(&mut proposer.miden_client, account.id(), tx_request)
            .await
            .unwrap();
    assert_eq!(expected_summary.block_number(), bound_block);
    let commitment = expected_summary.to_commitment();
    let expected_id = word_to_hex(&commitment);
    let proposer_hex = word_to_hex(&proposer_keys.commitment());

    // What GUARDIAN serves once the cosigner has signed.
    let mut signed_payload = ProposalPayload::new(&expected_summary)
        .with_signature(proposer_keys.as_ref(), commitment)
        .with_custom_metadata("b2agg".to_string())
        .with_required_signatures(2);
    signed_payload.signatures.push(DeltaSignature {
        signer_id: cosigner_keys.commitment_hex(),
        signature: ProposalSignature::Falcon {
            signature: cosigner_keys.sign_word_hex(commitment),
        },
    });
    let signed_delta = pending_proto_delta(
        &account,
        1,
        signed_payload.to_json().to_string(),
        &proposer_hex,
    );

    let service = MockGuardianService::default()
        .with_push_delta_proposal(Ok(PushDeltaProposalResponse {
            success: true,
            message: String::new(),
            commitment: expected_id.clone(),
            delta: None,
        }))
        .with_sign_delta_proposal(Ok(SignDeltaProposalResponse {
            success: true,
            message: String::new(),
            delta: Some(signed_delta.clone()),
        }))
        .with_push_delta(Ok(PushDeltaResponse {
            success: true,
            message: String::new(),
            delta: Some(ProtoDeltaObject {
                ack_scheme: Some("falcon".to_string()),
                ..signed_delta.clone()
            }),
            ack_sig: Some(guardian_keys.sign_word_hex(commitment)),
        }));
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    handle.set_persistent_get_pubkey(word_to_hex(&guardian_keys.commitment()));
    proposer
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    // The chain moves on and the proposer syncs past N before proposing.
    api.advance_blocks(3);
    proposer.miden_client.sync_state().await.unwrap();
    assert!(proposer.miden_client.get_sync_height().await.unwrap() > bound_block);

    let proposal = proposer
        .propose_custom_transaction(&request_bytes, "b2agg")
        .await
        .expect("a request bound to an older block is proposed at the tip");
    assert!(proposal.id.eq_ignore_ascii_case(&expected_id));
    assert_eq!(
        proposal.metadata.bound_block_num,
        Some(bound_block.as_u32()),
        "the proposal records the block the producer's request binds"
    );

    // A second client cosigns what the proposer pushed.
    let pushed = handle.pushed_proposals();
    assert_eq!(pushed.len(), 1, "one proposal pushed: {pushed:?}");
    let pushed_payload: serde_json::Value = serde_json::from_str(&pushed[0].delta_payload).unwrap();
    assert_eq!(
        pushed_payload["metadata"]["bound_block_num"].as_u64(),
        Some(u64::from(bound_block.as_u32())),
        "the pushed proposal names the block the producer's request binds"
    );
    handle.set_persistent_get_delta_proposal(GetDeltaProposalResponse {
        success: true,
        message: String::new(),
        proposal: Some(pending_proto_delta(
            &account,
            1,
            pushed[0].delta_payload.clone(),
            &proposer_hex,
        )),
    });
    let cosigner_dir = tempfile::tempdir().unwrap();
    let (mut cosigner, _cosigner_store) = offline_client_parts_with_keystore(
        cosigner_dir.path(),
        api.clone(),
        None,
        cosigner_keys.clone(),
    )
    .await;
    cosigner.set_node_rpc_client(api.clone());
    cosigner
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    cosigner.account = Some(MultisigAccount::new(account.clone()));
    cosigner
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    let signed = cosigner
        .sign_proposal(&expected_id)
        .await
        .expect("the cosigner signs");
    assert!(signed.status.is_ready(), "2-of-2 met: {:?}", signed.status);

    // The producer assembles the advice, gets the GUARDIAN ack and submits.
    handle.set_persistent_get_delta_proposal(GetDeltaProposalResponse {
        success: true,
        message: String::new(),
        proposal: Some(signed_delta),
    });
    let advice = proposer
        .prepare_custom_execution(&expected_id, &request_bytes)
        .await
        .expect("the producer's request reproduces the signed summary");
    let mut final_request = deserialize_transaction_request(&request_bytes).unwrap();
    final_request.advice_map_mut().extend(advice);
    // Submission seals the transaction against the validators' encryption key,
    // which the mock RPC does not serve.
    proposer
        .miden_client
        .seed_transaction_encryption_key(TransactionEncryptionKey::new_unattested(
            b"issue-538".to_vec(),
            SigningKey::new().public_key(),
            Word::default(),
        ))
        .await
        .unwrap();
    let nonce_before = proposer
        .miden_client
        .get_account(account.id())
        .await
        .unwrap()
        .expect("the proposer holds the account")
        .nonce()
        .as_canonical_u64();
    proposer
        .submit_transaction(&expected_id, final_request)
        .await
        .expect("the transaction is submitted");

    api.prove_block();
    proposer.miden_client.sync_state().await.unwrap();
    let transactions = proposer
        .miden_client
        .get_transactions(TransactionFilter::All)
        .await
        .unwrap();
    assert!(
        transactions
            .iter()
            .any(|tx| matches!(tx.status, TransactionStatus::Committed { .. })),
        "the transaction must land on chain: {transactions:?}"
    );
    let nonce_after = proposer
        .miden_client
        .get_account(account.id())
        .await
        .unwrap()
        .expect("the proposer holds the account")
        .nonce()
        .as_canonical_u64();
    assert_eq!(nonce_after, nonce_before + 1);
}

/// A multisig account already deployed on a chain that charges a verification
/// fee, holding enough of the fee asset to pay it. Like devnet's, the chain's
/// fee faucet has transfer policies, so its account ID enables asset callbacks
/// and the kernel loads it as a foreign account whenever its asset moves, the
/// fee payment included.
fn fee_charging_chain_holding(
    account: &mut miden_protocol::account::Account,
) -> Arc<miden_client::testing::mock::MockRpcApi> {
    use miden_protocol::asset::FungibleAsset;
    use miden_standards::account::policies::MintPolicy;

    let mut builder = miden_client::testing::MockChain::builder();
    let fee_faucet = builder
        .add_existing_network_faucet(
            "FEE",
            1_000_000_000,
            account.id(),
            Some(1_000_000),
            MintPolicy::allow_all(),
            [],
        )
        .expect("fee faucet is added");
    assert!(
        fee_faucet.id().asset_callback_flag().is_enabled(),
        "the fee faucet must enable asset callbacks, as devnet's does"
    );

    account
        .vault_mut()
        .add_asset(
            FungibleAsset::new(fee_faucet.id(), 1_000_000)
                .expect("fee asset")
                .into(),
        )
        .expect("fee asset is added");
    // A new account is built with an empty vault, and nonce zero marks it
    // undeployed; the chain takes this one as deployed and funded.
    account
        .set_nonce(miden_protocol::Felt::ONE)
        .expect("nonce is set");
    builder
        .add_account(account.clone())
        .expect("account is added");

    let chain = builder
        .fee_faucet_id(fee_faucet.id())
        .verification_base_fee(500)
        .build()
        .expect("mock chain builds");
    let api = Arc::new(miden_client::testing::mock::MockRpcApi::new(chain));
    api.advance_blocks(4);
    api
}

/// Issue #462: a node serves account state only ~50 blocks back, and every
/// fee-paying transaction loads the chain's fee faucet as a foreign account.
/// Re-executing a proposal at its anchor loaded the faucet at the bound block,
/// so once the node pruned that block's state nobody could verify or execute
/// the proposal. At the tip the faucet loads at the tip, and the summary still
/// reproduces because the request declares the block it binds.
///
/// The client here last synced at the bound block itself, the proposer's
/// position after proposing. An execution runs at the store's sync height, so
/// the listing has to bring the store to the tip before it re-executes, or the
/// faucet would load at the pruned block all the same.
#[tokio::test]
async fn proposal_verifies_at_the_tip_after_the_node_prunes_its_bound_block_state() {
    use crate::transaction::build_update_signers_transaction_request;

    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let mut account = multisig_account(signer_commitment, Word::from([9u32, 9, 9, 9]), 53);
    let api = fee_charging_chain_holding(&mut account);

    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api.clone(), None, keystore.clone()).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, true).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    // An add-cosigner proposal: the same signer set plus one, threshold kept.
    let signers = vec![signer_commitment, Word::from([7u32, 7, 7, 7])];
    let signers_hex: Vec<String> = signers.iter().map(word_to_hex).collect();
    let salt = Word::from([5u32, 6, 7, 8]);
    let auth_args = client.multisig_auth_args(salt, None, None).await.unwrap();
    let (tx_request, _) = build_update_signers_transaction_request(
        1,
        &signers,
        &auth_args,
        std::iter::empty(),
        client.key_manager.scheme(),
    )
    .unwrap();
    let chain_anchor = client
        .miden_client
        .chain_anchor_for_request(&tx_request)
        .await
        .unwrap();
    let tx_summary =
        execute_for_summary_at_tip(&mut client.miden_client, account.id(), tx_request.clone())
            .await
            .unwrap();
    let proposal_id = word_to_hex(&tx_summary.to_commitment());
    let bound_block = tx_summary.block_number();
    assert_eq!(chain_anchor.block_num(), bound_block);

    let payload = ProposalPayload::new(&tx_summary)
        .with_add_signer_metadata(1, signers_hex, word_to_hex(&salt))
        .with_required_signatures(1)
        .to_json()
        .to_string();
    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    handle.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        // The account is deployed at nonce 1, so the proposal is for nonce 2.
        proposals: vec![pending_proto_delta(
            &account,
            2,
            payload,
            &word_to_hex(&signer_commitment),
        )],
    });
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    // The chain moves on and the node prunes the bound block's account state,
    // the fee faucet's included. The client does not sync by itself.
    api.advance_blocks(5);
    api.prune_account_state_at(bound_block);
    assert_eq!(
        client.miden_client.get_sync_height().await.unwrap(),
        bound_block
    );

    // Control: re-executing at the proposal's anchor, as 0.18.0-rc.1 did, now
    // fails loading the fee faucet at the pruned block.
    let anchored_error = match client
        .miden_client
        .execute_transaction_at(account.id(), tx_request, chain_anchor)
        .await
    {
        Ok(_) => panic!("anchored re-execution must fail once the bound block is pruned"),
        Err(error) => format!("{error:?}"),
    };
    assert!(
        anchored_error.contains(&format!("no mock chain snapshot at block {bound_block}")),
        "the anchored re-execution must fail on the pruned foreign load: {anchored_error}"
    );

    // The strict listing re-derives the summary at the tip and verifies it.
    let proposals = client
        .list_proposals()
        .await
        .expect("the listing succeeds after the prune");
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    assert!(proposals[0].id.eq_ignore_ascii_case(&proposal_id));
    assert!(
        proposals[0].is_verified(),
        "the tip re-execution must reproduce the signed summary: {:?}",
        proposals[0].verification
    );
    assert!(
        client.miden_client.get_sync_height().await.unwrap() > bound_block,
        "the listing must have synced the store past the pruned block"
    );
}

/// A multisig summary reproduces only at a tip at or past its bound block. A
/// cosigner whose store is still below that block syncs once before the
/// re-execution instead of failing with "requested block N is after
/// transaction reference block M".
#[tokio::test]
async fn cosigner_below_the_bound_block_syncs_before_verifying() {
    use crate::transaction::build_update_signers_transaction_request;

    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let account = multisig_account(signer_commitment, Word::from([9u32, 9, 9, 9]), 54);
    let api = chain_with_notes(Vec::new());

    // The cosigner syncs first, then stops.
    let cosigner_dir = tempfile::tempdir().unwrap();
    let (mut cosigner, _cosigner_store) =
        offline_client_with_node_parts(cosigner_dir.path(), api.clone()).await;
    cosigner
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    cosigner.account = Some(MultisigAccount::new(account.clone()));
    cosigner.miden_client.sync_state().await.unwrap();
    let cosigner_height = cosigner.miden_client.get_sync_height().await.unwrap();

    // The chain moves on and the proposer builds at the new tip.
    api.advance_blocks(3);
    let proposer_dir = tempfile::tempdir().unwrap();
    let (mut proposer, _proposer_store) = offline_client_parts_with_keystore(
        proposer_dir.path(),
        api.clone(),
        None,
        keystore.clone(),
    )
    .await;
    proposer.set_node_rpc_client(api.clone());
    proposer
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    proposer.account = Some(MultisigAccount::new(account.clone()));
    proposer.miden_client.sync_state().await.unwrap();

    let signers = vec![signer_commitment, Word::from([7u32, 7, 7, 7])];
    let signers_hex: Vec<String> = signers.iter().map(word_to_hex).collect();
    let salt = Word::from([5u32, 6, 7, 8]);
    let auth_args = proposer.multisig_auth_args(salt, None, None).await.unwrap();
    let (tx_request, _) = build_update_signers_transaction_request(
        1,
        &signers,
        &auth_args,
        std::iter::empty(),
        proposer.key_manager.scheme(),
    )
    .unwrap();
    let tx_summary =
        execute_for_summary_at_tip(&mut proposer.miden_client, account.id(), tx_request)
            .await
            .unwrap();
    let bound_block = tx_summary.block_number();
    assert!(
        bound_block > cosigner_height,
        "the proposal must bind a block the cosigner has not synced to ({bound_block} vs \
         {cosigner_height})"
    );

    let payload = ProposalPayload::new(&tx_summary)
        .with_add_signer_metadata(1, signers_hex, word_to_hex(&salt))
        .with_required_signatures(1)
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
            payload,
            &word_to_hex(&signer_commitment),
        )],
    });
    cosigner
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    let proposals = cosigner
        .list_proposals()
        .await
        .expect("the cosigner syncs to the bound block and verifies");
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    assert!(
        proposals[0].is_verified(),
        "the cosigner must verify after syncing: {:?}",
        proposals[0].verification
    );
    assert!(cosigner.miden_client.get_sync_height().await.unwrap() >= bound_block);
}

/// A cosigner that has only just pulled the account has never synced its
/// Miden store, so it holds no block header to read the chain's fee faucet
/// from, and its sync height is below every bound block. Verification syncs
/// before it rebuilds the request rather than failing on the empty store.
#[tokio::test]
async fn cosigner_that_never_synced_verifies_after_syncing() {
    use crate::transaction::build_update_signers_transaction_request;

    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let account = multisig_account(signer_commitment, Word::from([9u32, 9, 9, 9]), 55);
    let api = chain_with_notes(Vec::new());

    let proposer_dir = tempfile::tempdir().unwrap();
    let (mut proposer, _proposer_store) = offline_client_parts_with_keystore(
        proposer_dir.path(),
        api.clone(),
        None,
        keystore.clone(),
    )
    .await;
    proposer.set_node_rpc_client(api.clone());
    proposer
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    proposer.account = Some(MultisigAccount::new(account.clone()));
    proposer.miden_client.sync_state().await.unwrap();

    let signers = vec![signer_commitment, Word::from([7u32, 7, 7, 7])];
    let signers_hex: Vec<String> = signers.iter().map(word_to_hex).collect();
    let salt = Word::from([5u32, 6, 7, 8]);
    let auth_args = proposer.multisig_auth_args(salt, None, None).await.unwrap();
    let (tx_request, _) = build_update_signers_transaction_request(
        1,
        &signers,
        &auth_args,
        std::iter::empty(),
        proposer.key_manager.scheme(),
    )
    .unwrap();
    let tx_summary =
        execute_for_summary_at_tip(&mut proposer.miden_client, account.id(), tx_request)
            .await
            .unwrap();

    let payload = ProposalPayload::new(&tx_summary)
        .with_add_signer_metadata(1, signers_hex, word_to_hex(&salt))
        .with_required_signatures(1)
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
            payload,
            &word_to_hex(&signer_commitment),
        )],
    });

    // The cosigner holds the account but has never synced.
    let cosigner_dir = tempfile::tempdir().unwrap();
    let (mut cosigner, _cosigner_store) =
        offline_client_with_node_parts(cosigner_dir.path(), api.clone()).await;
    cosigner
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    cosigner.account = Some(MultisigAccount::new(account.clone()));
    cosigner
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    let proposals = cosigner
        .list_proposals()
        .await
        .expect("the listing succeeds");
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    assert!(
        proposals[0].is_verified(),
        "a cosigner that never synced must sync and verify: {:?}",
        proposals[0].verification
    );
}

/// A listing client and an update-procedure-threshold summary bound to its
/// sync height, for the tests below that serve a hand-shaped payload.
async fn client_with_summary(
    seed: u8,
) -> (
    super::MultisigClient,
    tempfile::TempDir,
    miden_protocol::account::Account,
    Word,
    miden_protocol::transaction::TransactionSummary,
    Word,
) {
    use crate::procedures::ProcedureName;

    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let account = multisig_account(signer_commitment, Word::from([9u32, 9, 9, 9]), seed);
    let api = chain_with_notes(Vec::new());
    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api.clone(), None, keystore.clone()).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, true).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    let salt = Word::from([5u32, 6, 7, 8]);
    let auth_args = client.multisig_auth_args(salt, None, None).await.unwrap();
    let tx_type = TransactionType::UpdateProcedureThreshold {
        procedure: ProcedureName::SendAsset,
        new_threshold: 1,
    };
    let tx_request = build_final_transaction_request(
        &client.miden_client,
        &tx_type,
        &account,
        &auth_args,
        Vec::new(),
        None,
        None,
        client.key_manager.scheme(),
    )
    .await
    .unwrap();
    let tx_summary = execute_for_summary_at_tip(&mut client.miden_client, account.id(), tx_request)
        .await
        .unwrap();
    (client, dir, account, signer_commitment, tx_summary, salt)
}

/// Serves `payload` as the account's one pending proposal and lists it.
async fn list_served(
    client: &mut super::MultisigClient,
    account: &miden_protocol::account::Account,
    signer_commitment: Word,
    payload: serde_json::Value,
) -> Vec<crate::proposal::Proposal> {
    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(account));
    handle.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![pending_proto_delta(
            account,
            1,
            payload.to_string(),
            &word_to_hex(&signer_commitment),
        )],
    });
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    client
        .list_proposals()
        .await
        .expect("the listing reports the proposal")
}

/// A `bound_block_num` that disagrees with the block the signed summary binds
/// is refused by name. TypeScript cosigners rebuild at that number, so a wrong
/// one must not pass the Rust check silently.
#[tokio::test]
async fn proposal_whose_bound_block_num_disagrees_with_its_summary_is_refused() {
    use crate::procedures::ProcedureName;

    let (mut client, _dir, account, signer, tx_summary, salt) = client_with_summary(55).await;
    let bound_block = tx_summary.block_number().as_u32();
    let mut payload = ProposalPayload::new(&tx_summary)
        .with_procedure_threshold_metadata(ProcedureName::SendAsset, 1, word_to_hex(&salt))
        .with_required_signatures(1)
        .to_json();
    payload["metadata"]["bound_block_num"] = serde_json::json!(bound_block + 1);

    let proposals = list_served(&mut client, &account, signer, payload).await;
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    match &proposals[0].verification {
        crate::proposal::ProposalVerification::Failed { retryable, message } => {
            assert!(
                !retryable,
                "a wrong bound_block_num cannot clear on retry: {message}"
            );
            assert!(
                message.contains(&format!("bound_block_num {}", bound_block + 1))
                    && message.contains(&format!("binds block {bound_block}")),
                "the refusal must name both blocks: {message}"
            );
        }
        other => panic!("expected a failed verification, got {other:?}"),
    }
}

/// A proposal a 0.18.0 client made carries a `chain_anchor` and no
/// `bound_block_num`. It still verifies: the field is unknown now, and the
/// bound block is read from the signed summary.
#[tokio::test]
async fn proposal_made_by_a_0_18_client_still_verifies() {
    use crate::procedures::ProcedureName;

    let (mut client, _dir, account, signer, tx_summary, salt) = client_with_summary(57).await;
    let mut payload = ProposalPayload::new(&tx_summary)
        .with_procedure_threshold_metadata(ProcedureName::SendAsset, 1, word_to_hex(&salt))
        .with_required_signatures(1)
        .to_json();
    let metadata = payload["metadata"].as_object_mut().unwrap();
    metadata.remove("bound_block_num");
    metadata.insert(
        "chain_anchor".to_string(),
        serde_json::json!("bW9jay1jaGFpbi1hbmNob3I="),
    );

    let proposals = list_served(&mut client, &account, signer, payload).await;
    assert_eq!(proposals.len(), 1, "proposals: {proposals:?}");
    assert!(
        proposals[0].is_verified(),
        "a legacy proposal must still verify: {:?}",
        proposals[0].verification
    );
}

/// A client holding `account` on `api`, synced and pointed at a fresh mock
/// GUARDIAN that serves the account's state.
async fn client_on_mock_guardian(
    api: Arc<miden_client::testing::mock::MockRpcApi>,
    account: &miden_protocol::account::Account,
    keystore: Arc<GuardianKeyStore>,
) -> (
    super::MultisigClient,
    tempfile::TempDir,
    guardian_client::testing::mocks::MockGuardianHandle,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api.clone(), None, keystore).await;
    client.set_node_rpc_client(api);
    client.add_or_update_account(account, true).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_state(registered_state(account));
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    (client, dir, handle, endpoint)
}

/// Every built-in proposal records the block its summary binds as
/// `bound_block_num`, so a TypeScript cosigner can rebuild it. Rust readers
/// accept a missing value, so only this test pins the seven Rust writers. The
/// mock's canned push response carries no matching commitment, so creation
/// fails after the push under test.
#[tokio::test]
async fn built_in_proposals_write_the_bound_block_num() {
    use guardian_shared::FromJson;
    use miden_client::transaction::TransactionSummary;
    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };

    use super::test_support::test_wallet;
    use crate::procedures::ProcedureName;

    let keystore = Arc::new(GuardianKeyStore::generate());
    let second_signer = Word::from([7u32, 7, 7, 7]);
    let new_guardian = Word::from([4u32, 4, 4, 4]);
    let account = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
        1,
        vec![keystore.commitment(), second_signer],
        Word::from([9u32, 9, 9, 9]),
    ))
    .with_seed([56; 32])
    .build()
    .expect("1-of-2 multisig account builds");
    let note = p2id_note_for(&account, 31, NoteType::Private);
    let api = chain_with_notes(vec![RawOutputNote::Full(note.clone())]);
    let (mut client, _dir, handle, endpoint) =
        client_on_mock_guardian(api, &account, keystore.clone()).await;
    let node_rpc = client.node_rpc_client();
    crate::transaction::ensure_notes_authenticated(
        &mut client.miden_client,
        &node_rpc,
        std::slice::from_ref(&note),
    )
    .await
    .unwrap();
    // A switch first checks that its target serves the commitment it names,
    // and the target here is this same mock.
    handle.set_persistent_get_pubkey(word_to_hex(&new_guardian));

    // One proposal per builder in `transaction/builder.rs` (seven sites). A
    // P2ID transfer spends from the vault, so it runs on a funded account.
    for tx_type in [
        TransactionType::AddCosigner {
            new_commitment: Word::from([3u32, 3, 3, 3]),
        },
        TransactionType::RemoveCosigner {
            commitment: second_signer,
        },
        TransactionType::UpdateSigners {
            new_threshold: 2,
            signer_commitments: vec![keystore.commitment(), second_signer],
        },
        TransactionType::SwitchGuardian {
            new_endpoint: endpoint,
            new_commitment: new_guardian,
        },
        TransactionType::UpdateProcedureThreshold {
            procedure: ProcedureName::SendAsset,
            new_threshold: 1,
        },
        TransactionType::consume_notes_v2(vec![note.id()], vec![SerializedNote::from_note(&note)]),
    ] {
        let _ = client.propose_transaction(tx_type).await;
    }
    let mut pushed = handle.pushed_proposals();

    let mut funded = multisig_account(keystore.commitment(), Word::from([9u32, 9, 9, 9]), 58);
    let funded_api = fee_charging_chain_holding(&mut funded);
    let (mut payer, _payer_dir, payer_handle, _) =
        client_on_mock_guardian(funded_api, &funded, keystore).await;
    let fee_faucet = crate::transaction::synced_fee_faucet_id(&payer.miden_client)
        .await
        .unwrap();
    let _ = payer
        .propose_transaction(TransactionType::transfer(
            test_wallet(201).id(),
            fee_faucet,
            1000,
        ))
        .await;
    pushed.extend(payer_handle.pushed_proposals());

    assert_eq!(
        pushed.len(),
        7,
        "every builder reaches GUARDIAN: {pushed:?}"
    );
    for request in pushed {
        let payload: serde_json::Value = serde_json::from_str(&request.delta_payload).unwrap();
        let summary = TransactionSummary::from_json(&payload["tx_summary"]).unwrap();
        assert_eq!(
            payload["metadata"]["bound_block_num"].as_u64(),
            Some(u64::from(summary.block_number().as_u32())),
            "bound_block_num must name the block the summary binds: {payload}"
        );
        assert!(payload["metadata"].get("chain_anchor").is_none());
    }
}
