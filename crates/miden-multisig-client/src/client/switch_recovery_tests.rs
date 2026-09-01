//! Switch-guardian note-recovery integration tests (issue #417), driven
//! against the in-process mock GUARDIAN gRPC server from
//! `guardian_client::testing`.
//!
//! Unlike the options-slice unit tests in `note_recovery`, these run the
//! real pipeline: a genuine pending `consume_notes` v2 proposal — its
//! summary produced by the same abort-execution the binding check replays —
//! served by a mock pre-switch GUARDIAN, listed, binding-verified, and its
//! embedded note imported into a store that never held it, then shown to
//! survive the repoint to a new GUARDIAN that serves nothing.

use std::sync::Arc;

use base64::Engine as _;
use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use guardian_client::{
    AccountState, DeltaObject as ProtoDeltaObject, DeltaStatus, GetDeltaProposalResponse,
    GetDeltaProposalsResponse, GetStateResponse, PendingStatus, delta_status,
};
use guardian_shared::FromJson;
use miden_client::Serializable;
use miden_client::store::NoteFilter as StoreNoteFilter;
use miden_client::transaction::TransactionSummary;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::account::Account;
use miden_protocol::note::NoteType;

use super::note_recovery::NoteRecoveryOptions;
use super::proposal_note_import::NoteImportStatus;
use super::test_support::{
    chain_with_notes, offline_client_parts_with_keystore, offline_client_with_node_parts,
    p2id_note_for,
};
use crate::account::MultisigAccount;
use crate::execution::build_final_transaction_request;
use crate::keystore::{GuardianKeyStore, KeyManager};
use crate::payload::ProposalPayload;
use crate::proposal::{SerializedNote, TransactionType};
use crate::transaction::word_to_hex;

const BASE64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// A real 1-of-1 multisig-guardian account whose cosigner is `signer` — the
/// same construction `MultisigClient::create_account` performs, minus the
/// GUARDIAN pubkey fetch (the guardian commitment is fixed by the test).
fn multisig_account(signer: Word, guardian_commitment: Word, seed: u8) -> Account {
    let config = MultisigGuardianConfig::new(1, vec![signer], guardian_commitment);
    MultisigGuardianBuilder::new(config)
        .with_seed([seed; 32])
        .build()
        .expect("multisig account builds")
}

/// The canned `get_state` a mock GUARDIAN serves for `account`: the same
/// commitment the client holds, so guardian sync is a no-op.
fn registered_state(account: &Account) -> GetStateResponse {
    GetStateResponse {
        success: true,
        message: String::new(),
        state: Some(AccountState {
            account_id: account.id().to_string(),
            state_json: serde_json::json!({
                "data": BASE64.encode(account.to_bytes()),
            })
            .to_string(),
            commitment: word_to_hex(&account.to_commitment()),
            created_at: String::new(),
            updated_at: String::new(),
        }),
    }
}

/// Wraps a delta payload as the pending proto `DeltaObject` a GUARDIAN
/// listing would serve.
fn pending_proto_delta(
    account: &Account,
    nonce: u64,
    delta_payload: String,
    proposer_hex: &str,
) -> ProtoDeltaObject {
    ProtoDeltaObject {
        account_id: account.id().to_string(),
        nonce,
        prev_commitment: word_to_hex(&account.to_commitment()),
        delta_payload,
        new_commitment: String::new(),
        ack_sig: String::new(),
        candidate_at: String::new(),
        canonical_at: None,
        discarded_at: None,
        status: Some(DeltaStatus {
            status: Some(delta_status::Status::Pending(PendingStatus {
                timestamp: "2026-01-01T00:00:00Z".to_string(),
                proposer_id: proposer_hex.to_string(),
                cosigner_sigs: vec![],
            })),
            discard_reason: String::new(),
            retain_reason: String::new(),
        }),
        ack_pubkey: None,
        ack_scheme: None,
    }
}

/// Issue #417 continuity: a note embedded in a proposal still pending on the
/// pre-switch GUARDIAN is imported by `preserve_pre_switch_proposal_notes`
/// into a store that never held it — through the real listing, summary
/// binding re-execution, and import — and remains present (proof-backed,
/// i.e. consumable once verified by sync) after the client repoints to a new
/// GUARDIAN that serves no proposals at all.
#[tokio::test]
async fn pre_switch_import_preserves_pending_proposal_notes_across_the_repoint() {
    // The proposer client, whose key is the account's one cosigner.
    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let guardian_commitment = Word::from([9u32, 9, 9, 9]);
    let account = multisig_account(signer_commitment, guardian_commitment, 47);

    // A private P2ID note for the account, committed on the mock chain so
    // the import can fetch its inclusion proof. The chain carries only the
    // note's commitment for a private note, so no client discovers its body
    // by syncing — exactly why the proposal embeds the serialized bytes.
    let note = p2id_note_for(&account, 7, NoteType::Private);
    let api = chain_with_notes(vec![miden_protocol::transaction::RawOutputNote::Full(
        note.clone(),
    )]);

    // The author builds the pending consume-notes v2 proposal from the
    // embedded bytes exactly the way binding verification replays it: the
    // same request builder and an abort-execution for the summary, from a
    // store that does not hold the note's proof — the self-contained v2
    // reconstruction path every verifier can reproduce.
    let dir1 = tempfile::tempdir().unwrap();
    let (mut author, _store1) =
        offline_client_parts_with_keystore(dir1.path(), api.clone(), None, keystore.clone()).await;
    author.set_node_rpc_client(api.clone());
    author.add_or_update_account(&account, true).await.unwrap();
    author.account = Some(MultisigAccount::new(account.clone()));
    author.miden_client.sync_state().await.unwrap();

    let salt = Word::from([5u32, 6, 7, 8]);
    let tx_type =
        TransactionType::consume_notes_v2(vec![note.id()], vec![SerializedNote::from_note(&note)]);
    let tx_request = build_final_transaction_request(
        &author.miden_client,
        &tx_type,
        &account,
        salt,
        Vec::new(),
        None,
        Some(&[]),
        author.key_manager.scheme(),
    )
    .await
    .unwrap();
    let (tx_summary, chain_anchor) =
        crate::transaction::execute_for_summary(&mut author.miden_client, account.id(), tx_request)
            .await
            .unwrap();

    let delta_payload = ProposalPayload::new(&tx_summary)
        .with_note_consumption_metadata_v2(
            vec![note.id().to_hex()],
            vec![SerializedNote::from_note(&note).into_inner()],
            word_to_hex(&salt),
        )
        .with_required_signatures(1)
        .with_chain_anchor(crate::transaction::chain_anchor_to_base64(&chain_anchor))
        .to_json()
        .to_string();

    // Mock pre-switch GUARDIAN A serving the registered state and the
    // pending proposal; mock post-switch GUARDIAN B serving nothing.
    let service_a = MockGuardianService::default();
    let handle_a = service_a.handle();
    let endpoint_a = start_mock_server(service_a).await.unwrap();
    let service_b = MockGuardianService::default();
    let handle_b = service_b.handle();
    let endpoint_b = start_mock_server(service_b).await.unwrap();
    handle_a.set_persistent_get_state(registered_state(&account));
    handle_a.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![pending_proto_delta(
            &account,
            1,
            delta_payload,
            &word_to_hex(&signer_commitment),
        )],
    });
    handle_b.set_persistent_get_state(registered_state(&account));

    // The switch executor: same account, fresh store — it has NEVER seen the
    // note. This is the state the pre-switch import must preserve into.
    let dir2 = tempfile::tempdir().unwrap();
    let (mut executor, _store2) = offline_client_with_node_parts(dir2.path(), api.clone()).await;
    executor
        .set_guardian_endpoint(&endpoint_a, false)
        .await
        .unwrap();
    executor
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    executor.account = Some(MultisigAccount::new(account.clone()));
    executor.miden_client.sync_state().await.unwrap();
    let before = executor
        .miden_client
        .get_input_notes(StoreNoteFilter::All)
        .await
        .unwrap();
    assert!(
        before.is_empty(),
        "the executor store must start without the note"
    );

    // The pre-switch import runs the real listing + binding + import.
    let report = executor
        .preserve_pre_switch_proposal_notes()
        .await
        .expect("best-effort flow reports instead of erroring");
    assert!(
        report.problems.is_empty(),
        "problems: {:?}",
        report.problems
    );
    let outcomes = report.proposal_import.expect("proposal import ran");
    assert_eq!(outcomes.len(), 1, "outcomes: {outcomes:?}");
    assert_eq!(
        outcomes[0].status,
        NoteImportStatus::Imported,
        "{outcomes:?}"
    );
    assert_eq!(outcomes[0].identifier, note.id().to_hex());
    assert_eq!(report.imported, 1);

    // Repoint to the new GUARDIAN, which serves no proposals: the note must
    // already be in the local store, proof-backed — the pre-switch import
    // was its only way in.
    executor
        .set_guardian_endpoint(&endpoint_b, false)
        .await
        .unwrap();
    let post_switch = executor
        .recover_notes(Some(NoteRecoveryOptions::for_guardian_switch()))
        .await
        .unwrap();
    assert_eq!(
        post_switch.proposal_import,
        Some(vec![]),
        "the new GUARDIAN has nothing to recover from"
    );

    let records = executor
        .miden_client
        .get_input_notes(StoreNoteFilter::All)
        .await
        .unwrap();
    assert_eq!(records.len(), 1, "the note survived the repoint");
    assert!(
        records[0].inclusion_proof().is_some(),
        "the record is proof-backed, so the next verifying sync makes it consumable"
    );
}

/// Issue #417 wiring: the online `execute_proposal` switch path invokes the
/// pre-switch proposal-note import against the old GUARDIAN — after the
/// proposal fetch, before the switch-delta push (and so before anything
/// switch-related lands on the old GUARDIAN, and before the switch
/// transaction executes). Asserted from the mock GUARDIAN's own
/// cross-endpoint call log, so removing the `execute_proposal` call site
/// fails this test regardless of client-side logging.
#[tokio::test]
async fn execute_proposal_runs_the_pre_switch_import_before_the_delta_push() {
    let keystore = Arc::new(GuardianKeyStore::generate());
    let signer_commitment = keystore.commitment();
    let guardian_commitment = Word::from([9u32, 9, 9, 9]);
    let account = multisig_account(signer_commitment, guardian_commitment, 53);

    let api = chain_with_notes(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir.path(), api.clone(), None, keystore.clone()).await;
    client.set_node_rpc_client(api.clone());
    client.add_or_update_account(&account, false).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();

    // Mock GUARDIAN B: the switch target, serving the pubkey the proposal
    // commits to (endpoint-commitment verification hits it twice: once at
    // offline creation, once inside finalize).
    let new_guardian_commitment = Word::from([11u32, 12, 13, 14]);
    let service_b = MockGuardianService::default();
    let handle_b = service_b.handle();
    let endpoint_b = start_mock_server(service_b).await.unwrap();
    handle_b.set_persistent_get_pubkey(word_to_hex(&new_guardian_commitment));

    // A real ready switch proposal: offline creation produces the signed
    // summary from this exact account + chain state, so the online path's
    // binding verification reproduces it.
    let exported = client
        .create_proposal_offline(TransactionType::switch_guardian(
            endpoint_b.clone(),
            new_guardian_commitment,
        ))
        .await
        .unwrap();
    let switch_id = exported.id.clone();
    let signature = &exported.signatures[0];

    let switch_summary =
        TransactionSummary::from_json(&exported.tx_summary).expect("exported summary parses");
    let mut switch_payload = ProposalPayload::new(&switch_summary)
        .with_guardian_update_metadata(
            word_to_hex(&new_guardian_commitment),
            endpoint_b.clone(),
            exported.metadata.salt_hex.clone().expect("salt exported"),
        )
        .with_required_signatures(1)
        .with_chain_anchor(
            exported
                .metadata
                .chain_anchor
                .clone()
                .expect("anchor exported"),
        );
    switch_payload
        .signatures
        .push(guardian_shared::DeltaSignature {
            signer_id: signature.signer_commitment.clone(),
            signature: guardian_shared::ProposalSignature::Falcon {
                signature: signature.signature.clone(),
            },
        });
    let switch_payload = switch_payload.to_json().to_string();

    // Mock pre-switch GUARDIAN A serving state, the switch proposal (fetch +
    // listing), and accepting the delta push.
    let service_a = MockGuardianService::default();
    let handle_a = service_a.handle();
    let endpoint_a = start_mock_server(service_a).await.unwrap();
    handle_a.set_persistent_get_state(registered_state(&account));
    let switch_delta = pending_proto_delta(
        &account,
        1,
        switch_payload,
        &word_to_hex(&signer_commitment),
    );
    handle_a.set_persistent_get_delta_proposal(GetDeltaProposalResponse {
        success: true,
        message: String::new(),
        proposal: Some(switch_delta.clone()),
    });
    handle_a.set_persistent_get_delta_proposals(GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![switch_delta],
    });
    client
        .set_guardian_endpoint(&endpoint_a, false)
        .await
        .unwrap();

    // Drive the online switch execution. Everything #417 touches runs for
    // real — binding verification, signature advice, the pre-switch import,
    // the delta push, chain execution and proving — and the run dies only at
    // final submission, on a mock-infrastructure limit (`MockRpcApi` serves
    // no transaction encryption key). The wiring under test is asserted from
    // the mock GUARDIAN's call log, which is complete by then.
    let result = client.execute_proposal(&switch_id).await;

    let calls = handle_a.calls();
    let listing_at = calls
        .iter()
        .position(|call| call == "get_delta_proposals")
        .unwrap_or_else(|| {
            panic!("pre-switch import listing must run; calls: {calls:?}; result: {result:?}")
        });
    let push_at = calls
        .iter()
        .position(|call| call == "push_delta")
        .unwrap_or_else(|| panic!("switch-delta push must run; calls: {calls:?}"));
    assert!(
        listing_at < push_at,
        "the import listing must precede the switch-delta push; calls: {calls:?}"
    );

    if let Err(error) = result {
        // The only tolerated failure is the known mock-infrastructure limit
        // at final submission; anything else would mean a stage regressed.
        assert!(
            error.to_string().contains("transaction encryption key"),
            "unexpected execute_proposal failure: {error}"
        );
    }
}
