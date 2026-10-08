//! Guardian-executable proposal creation against the in-process mock GUARDIAN and a mock chain:
//! what a proposal stores, what its summary binds, and that the stored request reproduces it.

use std::sync::Arc;

use guardian_client::testing::mocks::{MockGuardianService, start_mock_server};
use guardian_shared::FromJson;
use guardian_shared::request_envelope::TransactionRequestEnvelope;
use miden_client::transaction::TransactionSummary;
use miden_protocol::Word;

use super::test_support::{
    chain_with_notes, multisig_account, offline_client_parts_with_keystore, registered_state,
};
use crate::account::MultisigAccount;
use crate::client::MultisigClient;
use crate::keystore::{GuardianKeyStore, KeyManager};
use crate::procedures::ProcedureName;
use crate::proposal::TransactionType;
use crate::transaction::{
    GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA, GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA,
    ProposalExecutionMode, deserialize_transaction_request, execute_for_summary_at_tip,
    summary_approval_expiration_block_num,
};

struct Proposed {
    client: MultisigClient,
    payload: serde_json::Value,
    summary: TransactionSummary,
}

/// Proposes the same threshold change under `mode`, returning the payload the client pushed.
/// The push fails after it is recorded, because the mock answers with a canned commitment.
async fn propose(dir: &std::path::Path, mode: ProposalExecutionMode) -> Proposed {
    propose_as(
        dir,
        mode,
        TransactionType::UpdateProcedureThreshold {
            procedure: ProcedureName::ReceiveAsset,
            new_threshold: 1,
        },
    )
    .await
}

async fn propose_as(
    dir: &std::path::Path,
    mode: ProposalExecutionMode,
    transaction_type: TransactionType,
) -> Proposed {
    propose_on(dir, mode, AccountShape::OneSigner, |_| transaction_type).await
}

/// The account a family needs to be proposable at all.
#[derive(Clone, Copy)]
enum AccountShape {
    OneSigner,
    /// A 1-of-2 account whose second signer can be removed or the threshold raised.
    TwoSigners,
    /// An existing 1-of-1 account holding fungible assets to send.
    Funded,
}

impl AccountShape {
    fn build(self, signer: Word) -> (miden_protocol::account::Account, Vec<Word>) {
        use miden_confidential_contracts::multisig_guardian::{
            MultisigGuardianBuilder, MultisigGuardianConfig,
        };
        let guardian = Word::from([9u32, 9, 9, 9]);
        match self {
            AccountShape::OneSigner => (multisig_account(signer, guardian, 61), vec![signer]),
            AccountShape::TwoSigners => {
                let second = GuardianKeyStore::generate().commitment();
                let account = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
                    1,
                    vec![signer, second],
                    guardian,
                ))
                .with_seed([63; 32])
                .build()
                .unwrap();
                (account, vec![signer, second])
            }
            AccountShape::Funded => {
                let existing = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
                    1,
                    vec![signer],
                    guardian,
                ))
                .with_seed([64; 32])
                .build_existing()
                .unwrap();
                let (id, _vault, storage, code, nonce, _seed) = existing.into_parts();
                let vault = miden_protocol::asset::AssetVault::new(&[
                    miden_protocol::asset::FungibleAsset::mock(1_000),
                ])
                .unwrap();
                (
                    miden_protocol::account::Account::new_unchecked(
                        id, vault, storage, code, nonce, None,
                    ),
                    vec![signer],
                )
            }
        }
    }
}

/// Proposes the transaction `transaction_type` builds for an account of `shape`, given the
/// account's signers.
async fn propose_on(
    dir: &std::path::Path,
    mode: ProposalExecutionMode,
    shape: AccountShape,
    transaction_type: impl FnOnce(Vec<Word>) -> TransactionType,
) -> Proposed {
    let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let (account, signers) = shape.build(keystore.commitment());
    let transaction_type = transaction_type(signers);
    let api = chain_with_notes(vec![]);
    let (mut client, _store) =
        offline_client_parts_with_keystore(dir, api.clone(), None, keystore).await;
    client.set_node_rpc_client(api);
    client.add_or_update_account(&account, false).await.unwrap();
    client.account = Some(MultisigAccount::new(account.clone()));
    client.miden_client.sync_state().await.unwrap();
    client.execution_mode = mode;

    let guardian = MockGuardianService::default();
    let handle = guardian.handle();
    let endpoint = start_mock_server(guardian).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    client
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();

    let _ = client.propose_transaction(transaction_type).await;
    let pushed = handle.pushed_proposals();
    assert_eq!(pushed.len(), 1, "the proposal was pushed once");
    let payload: serde_json::Value = serde_json::from_str(&pushed[0].delta_payload).unwrap();
    let summary = TransactionSummary::from_json(&payload["tx_summary"]).unwrap();
    Proposed {
        client,
        payload,
        summary,
    }
}

#[tokio::test]
async fn a_default_client_stores_no_request_and_signs_no_extra_bound() {
    let dir = tempfile::tempdir().unwrap();
    let proposed = propose(dir.path(), ProposalExecutionMode::SelfExecuted).await;
    let keys: Vec<&str> = proposed
        .payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["metadata", "signatures", "tx_summary"]);
    assert_eq!(proposed.summary.expiration_delta(), 0);
    assert_eq!(
        summary_approval_expiration_block_num(&proposed.summary),
        None
    );
}

#[tokio::test]
async fn a_guardian_executable_client_stores_a_request_that_reproduces_its_summary() {
    let dir = tempfile::tempdir().unwrap();
    let mut proposed = propose(dir.path(), ProposalExecutionMode::GuardianExecutable).await;

    let envelope: TransactionRequestEnvelope =
        serde_json::from_value(proposed.payload["transaction_request"].clone()).unwrap();
    assert_eq!(envelope.protocol_line, "0.17");
    assert!(
        envelope.checksum.starts_with("0x")
            && envelope.checksum == envelope.checksum.to_lowercase()
    );
    let bytes = envelope.verified_bytes().unwrap();

    let summary = &proposed.summary;
    assert_eq!(
        summary.expiration_delta(),
        GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA.get(),
        "the transaction expiration is signed"
    );
    assert_eq!(
        summary_approval_expiration_block_num(summary).map(|block| block.as_u32()),
        Some(summary.block_number().as_u32() + GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA.get()),
        "the approval expiration defaults to its Guardian-executable window"
    );

    let request = deserialize_transaction_request(&bytes).unwrap();
    assert!(request.block_numbers().contains(&summary.block_number()));
    let account_id = proposed.client.require_account().unwrap().id();
    let reproduced =
        execute_for_summary_at_tip(&mut proposed.client.miden_client, account_id, request)
            .await
            .unwrap();
    assert_eq!(
        reproduced.to_commitment(),
        summary.to_commitment(),
        "the stored request alone reproduces the signed summary, so the proposal id is unchanged"
    );
}

/// Starts a GUARDIAN a switch can target, serving the commitment the proposal names.
async fn switch_target(commitment: Word) -> String {
    let service = MockGuardianService::default();
    let handle = service.handle();
    let endpoint = start_mock_server(service).await.unwrap();
    handle.set_persistent_get_pubkey(crate::transaction::word_to_hex(&commitment));
    endpoint
}

#[tokio::test]
async fn every_family_a_one_signer_account_can_propose_carries_both_bounds_and_reproduces() {
    let new_guardian = Word::from([11u32, 12, 13, 14]);
    let families = [
        (
            "update_procedure_threshold",
            TransactionType::UpdateProcedureThreshold {
                procedure: ProcedureName::SendAsset,
                new_threshold: 1,
            },
        ),
        (
            "add_signer",
            TransactionType::AddCosigner {
                new_commitment: GuardianKeyStore::generate().commitment(),
            },
        ),
        (
            "switch_guardian",
            TransactionType::switch_guardian(switch_target(new_guardian).await, new_guardian),
        ),
    ];
    for (family, transaction_type) in families {
        let dir = tempfile::tempdir().unwrap();
        let mut proposed = propose_as(
            dir.path(),
            ProposalExecutionMode::GuardianExecutable,
            transaction_type,
        )
        .await;
        let summary = &proposed.summary;
        assert_eq!(
            summary.expiration_delta(),
            GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA.get(),
            "{family}: the transaction expiration is signed"
        );
        assert_eq!(
            summary_approval_expiration_block_num(summary).map(|block| block.as_u32()),
            Some(
                summary.block_number().as_u32()
                    + GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA.get()
            ),
            "{family}: the approval expiration is signed"
        );
        let envelope: TransactionRequestEnvelope =
            serde_json::from_value(proposed.payload["transaction_request"].clone())
                .unwrap_or_else(|e| panic!("{family}: no stored request: {e}"));
        let bytes = envelope.verified_bytes().unwrap();
        let account_id = proposed.client.require_account().unwrap().id();
        let reproduced = execute_for_summary_at_tip(
            &mut proposed.client.miden_client,
            account_id,
            deserialize_transaction_request(&bytes).unwrap(),
        )
        .await
        .unwrap_or_else(|e| panic!("{family}: the stored request does not execute: {e}"));
        assert_eq!(
            reproduced.to_commitment(),
            summary.to_commitment(),
            "{family}: the stored request reproduces the signed summary"
        );
    }
}

type FamilyBuilder = Box<dyn FnOnce(Vec<Word>) -> TransactionType>;

#[tokio::test]
async fn the_signer_set_and_payment_families_carry_both_bounds_and_reproduce() {
    let asset = miden_protocol::asset::FungibleAsset::mock(1);
    let families: [(&str, AccountShape, FamilyBuilder); 3] = [
        (
            "remove_signer",
            AccountShape::TwoSigners,
            Box::new(|signers| TransactionType::RemoveCosigner {
                commitment: signers[1],
            }),
        ),
        (
            "change_threshold",
            AccountShape::TwoSigners,
            Box::new(|signers| TransactionType::UpdateSigners {
                new_threshold: 2,
                signer_commitments: signers,
            }),
        ),
        (
            "p2id",
            AccountShape::Funded,
            Box::new(move |_| TransactionType::P2ID {
                recipient: super::test_support::test_wallet(201).id(),
                faucet_id: asset.unwrap_fungible().faucet_id(),
                amount: 1,
                note_type: miden_protocol::note::NoteType::Public,
                heights: Default::default(),
            }),
        ),
    ];
    for (family, shape, transaction_type) in families {
        let dir = tempfile::tempdir().unwrap();
        let mut proposed = propose_on(
            dir.path(),
            ProposalExecutionMode::GuardianExecutable,
            shape,
            transaction_type,
        )
        .await;
        assert_guardian_executable(family, &mut proposed).await;
    }
}

async fn assert_guardian_executable(family: &str, proposed: &mut Proposed) {
    let summary = &proposed.summary;
    assert_eq!(
        summary.expiration_delta(),
        GUARDIAN_EXECUTABLE_TX_EXPIRATION_DELTA.get(),
        "{family}: the transaction expiration is signed"
    );
    assert_eq!(
        summary_approval_expiration_block_num(summary).map(|block| block.as_u32()),
        Some(summary.block_number().as_u32() + GUARDIAN_EXECUTABLE_APPROVAL_EXPIRATION_DELTA.get()),
        "{family}: the approval expiration is signed"
    );
    let envelope: TransactionRequestEnvelope =
        serde_json::from_value(proposed.payload["transaction_request"].clone())
            .unwrap_or_else(|e| panic!("{family}: no stored request: {e}"));
    let bytes = envelope.verified_bytes().unwrap();
    let account_id = proposed.client.require_account().unwrap().id();
    let reproduced = execute_for_summary_at_tip(
        &mut proposed.client.miden_client,
        account_id,
        deserialize_transaction_request(&bytes).unwrap(),
    )
    .await
    .unwrap_or_else(|e| panic!("{family}: the stored request does not execute: {e}"));
    assert_eq!(
        reproduced.to_commitment(),
        summary.to_commitment(),
        "{family}: the stored request reproduces the signed summary"
    );
}

#[tokio::test]
async fn the_same_effects_in_the_two_modes_are_different_proposals() {
    let (self_dir, guardian_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let self_executed = propose(self_dir.path(), ProposalExecutionMode::SelfExecuted).await;
    let guardian = propose(
        guardian_dir.path(),
        ProposalExecutionMode::GuardianExecutable,
    )
    .await;
    assert_ne!(
        self_executed.summary.to_commitment(),
        guardian.summary.to_commitment()
    );
}

struct PinnedProposal {
    api: Arc<miden_client::testing::mock::MockRpcApi>,
    account: miden_protocol::account::Account,
    note: miden_protocol::note::Note,
    signer_commitment: Word,
    cosigner_keystore: Arc<dyn KeyManager>,
    payload: serde_json::Value,
    summary: TransactionSummary,
    request_bytes: Vec<u8>,
}

/// A 1-of-2 account's Guardian-executable consume-notes proposal of one private note, as its
/// proposer pushed it, with the other signer's key for cosigning.
async fn pinned_consume_proposal(dir: &std::path::Path) -> PinnedProposal {
    use miden_protocol::note::NoteType;
    use miden_protocol::transaction::RawOutputNote;

    use miden_confidential_contracts::multisig_guardian::{
        MultisigGuardianBuilder, MultisigGuardianConfig,
    };

    let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let cosigner_keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
    let account = MultisigGuardianBuilder::new(MultisigGuardianConfig::new(
        1,
        vec![keystore.commitment(), cosigner_keystore.commitment()],
        Word::from([9u32, 9, 9, 9]),
    ))
    .with_seed([62; 32])
    .build()
    .unwrap();
    let note = super::test_support::p2id_note_for(&account, 11, NoteType::Private);
    let api = chain_with_notes(vec![RawOutputNote::Full(note.clone())]);

    let (mut proposer, _store) =
        offline_client_parts_with_keystore(dir, api.clone(), None, keystore.clone()).await;
    proposer.set_node_rpc_client(api.clone());
    proposer
        .add_or_update_account(&account, true)
        .await
        .unwrap();
    proposer.account = Some(MultisigAccount::new(account.clone()));
    proposer.miden_client.sync_state().await.unwrap();
    proposer.execution_mode = ProposalExecutionMode::GuardianExecutable;
    let node_rpc = proposer.node_rpc_client();
    crate::transaction::ensure_notes_authenticated(
        &mut proposer.miden_client,
        &node_rpc,
        std::slice::from_ref(&note),
    )
    .await
    .unwrap();

    let guardian = MockGuardianService::default();
    let handle = guardian.handle();
    let endpoint = start_mock_server(guardian).await.unwrap();
    handle.set_persistent_get_state(registered_state(&account));
    proposer
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    let _ = proposer
        .propose_transaction(TransactionType::consume_notes(vec![note.id()]))
        .await;
    let pushed = handle.pushed_proposals();
    assert_eq!(pushed.len(), 1, "the proposal was pushed once");
    let payload: serde_json::Value = serde_json::from_str(&pushed[0].delta_payload).unwrap();
    let summary = TransactionSummary::from_json(&payload["tx_summary"]).unwrap();
    let envelope: TransactionRequestEnvelope =
        serde_json::from_value(payload["transaction_request"].clone()).unwrap();
    let request_bytes = envelope.verified_bytes().unwrap();
    PinnedProposal {
        api,
        account,
        note,
        signer_commitment: keystore.commitment(),
        cosigner_keystore,
        payload,
        summary,
        request_bytes,
    }
}

/// A Guardian-executable consume-notes proposal pins every note with its inclusion proof, so a
/// party whose store has never seen the notes, which is Guardian's position, reproduces the
/// signed summary from the stored request alone, at its own later tip.
#[tokio::test]
async fn a_pinned_consume_request_reproduces_on_a_store_that_never_saw_the_notes() {
    let dir = tempfile::tempdir().unwrap();
    let proposed = pinned_consume_proposal(dir.path()).await;

    proposed.api.advance_blocks(3);
    let executor_dir = tempfile::tempdir().unwrap();
    let (mut executor, _store) = offline_client_parts_with_keystore(
        executor_dir.path(),
        proposed.api.clone(),
        None,
        Arc::new(GuardianKeyStore::generate()),
    )
    .await;
    executor
        .add_or_update_account(&proposed.account, true)
        .await
        .unwrap();
    executor.miden_client.sync_state().await.unwrap();
    assert!(
        executor
            .miden_client
            .get_input_note(proposed.note.id())
            .await
            .unwrap()
            .is_none(),
        "the executing store has never seen the note"
    );

    let reproduced = execute_for_summary_at_tip(
        &mut executor.miden_client,
        proposed.account.id(),
        deserialize_transaction_request(&proposed.request_bytes).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(reproduced.to_commitment(), proposed.summary.to_commitment());
}

/// A self-executed cosigner lists, verifies and exports a Guardian-executable proposal exactly as
/// it would any other: its rebuild takes the transaction expiration from the signed summary.
#[tokio::test]
async fn a_default_cosigner_lists_verifies_signs_and_exports_a_guardian_executable_proposal() {
    let dir = tempfile::tempdir().unwrap();
    let proposed = pinned_consume_proposal(dir.path()).await;
    let proposal_id = proposed.summary.to_commitment().to_hex();
    let served = || {
        super::test_support::pending_proto_delta(
            &proposed.account,
            1,
            proposed.payload.to_string(),
            &crate::transaction::word_to_hex(&proposed.signer_commitment),
        )
    };

    let guardian = MockGuardianService::default().with_sign_delta_proposal(Ok(
        guardian_client::SignDeltaProposalResponse {
            success: true,
            message: String::new(),
            delta: Some(served()),
        },
    ));
    let handle = guardian.handle();
    let endpoint = start_mock_server(guardian).await.unwrap();
    handle.set_persistent_get_state(registered_state(&proposed.account));
    handle.set_persistent_get_delta_proposals(guardian_client::GetDeltaProposalsResponse {
        success: true,
        message: String::new(),
        proposals: vec![served()],
    });

    handle.set_persistent_get_delta_proposal(guardian_client::GetDeltaProposalResponse {
        success: true,
        message: String::new(),
        proposal: Some(served()),
    });

    proposed.api.advance_blocks(5);
    let cosigner_dir = tempfile::tempdir().unwrap();
    let (mut cosigner, _store) = offline_client_parts_with_keystore(
        cosigner_dir.path(),
        proposed.api.clone(),
        None,
        proposed.cosigner_keystore.clone(),
    )
    .await;
    cosigner.set_node_rpc_client(proposed.api.clone());
    cosigner
        .set_guardian_endpoint(&endpoint, false)
        .await
        .unwrap();
    cosigner
        .add_or_update_account(&proposed.account, true)
        .await
        .unwrap();
    cosigner.account = Some(MultisigAccount::new(proposed.account.clone()));
    cosigner.miden_client.sync_state().await.unwrap();
    assert_eq!(
        cosigner.execution_mode(),
        ProposalExecutionMode::SelfExecuted
    );

    let proposals = cosigner
        .list_proposals()
        .await
        .expect("the strict listing verifies a Guardian-executable proposal");
    assert_eq!(proposals.len(), 1);
    assert!(
        proposals[0].id.eq_ignore_ascii_case(&proposal_id),
        "{} vs {proposal_id}",
        proposals[0].id
    );
    assert!(proposals[0].is_verified(), "{:?}", proposals[0]);
    assert!(
        cosigner
            .known_proposals
            .admit(
                proposed.account.id(),
                &proposals[0].id,
                crate::local_execution::GuardianExecutionRequest::default(),
            )
            .is_ok(),
        "a listed proposal is held for a GUARDIAN execution request"
    );

    let signed = cosigner
        .sign_proposal(&proposals[0].id)
        .await
        .expect("a default cosigner signs a Guardian-executable proposal");
    assert_eq!(signed.id, proposals[0].id);
    assert!(signed.is_verified(), "{signed:?}");

    let exported = cosigner
        .export_proposal_to_string(&proposals[0].id)
        .await
        .unwrap();
    assert!(exported.contains(&proposals[0].id));

    cosigner.known_proposals = crate::local_execution::KnownProposals::default();
    let admit = |cosigner: &MultisigClient| {
        cosigner.known_proposals.admit(
            proposed.account.id(),
            &proposals[0].id,
            crate::local_execution::GuardianExecutionRequest::default(),
        )
    };
    assert!(
        matches!(
            admit(&cosigner),
            Err(crate::error::MultisigError::ProposalNotHeldLocally { .. })
        ),
        "a client that never saw the proposal fails closed"
    );
    cosigner
        .import_proposal_from_string(&exported)
        .await
        .expect("the exported proposal imports");
    assert!(
        admit(&cosigner).is_ok(),
        "an imported proposal is held for a GUARDIAN execution request"
    );
}

mod local_execution {
    use guardian_client::ExecutionEnvelope;
    use guardian_client::testing::mocks::MockGuardianHandle;
    use miden_protocol::account::AccountId;
    use miden_protocol::note::NoteType;

    use super::*;
    use crate::error::MultisigError;
    use crate::local_execution::{GuardianExecutionRequest, LocalExecutionReason};
    use crate::proposal::P2ideHeights;

    const PROPOSAL_ID: &str = "0xabc123";

    struct Requester {
        client: MultisigClient,
        handle: MockGuardianHandle,
        account_id: AccountId,
        _dir: tempfile::TempDir,
    }

    impl Requester {
        async fn start() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let keystore: Arc<dyn KeyManager> = Arc::new(GuardianKeyStore::generate());
            let account = multisig_account(keystore.commitment(), Word::from([9u32, 9, 9, 9]), 71);
            let (mut client, _store) = offline_client_parts_with_keystore(
                dir.path(),
                chain_with_notes(vec![]),
                None,
                keystore,
            )
            .await;
            client.account = Some(MultisigAccount::new(account.clone()));
            client.execution_mode = ProposalExecutionMode::GuardianExecutable;

            let accepted = ExecutionEnvelope {
                account_id: account.id().to_string(),
                proposal_id: PROPOSAL_ID.to_string(),
                state: "pending".to_string(),
                error: None,
                delta_nonce: None,
                newly_accepted: true,
                proposal_exists: true,
                ignored_signatures: 0,
                updated_at: "2026-10-07T12:00:00Z".to_string(),
            };
            let guardian = MockGuardianService::default().with_execution(Ok(accepted));
            let handle = guardian.handle();
            let endpoint = start_mock_server(guardian).await.unwrap();
            client
                .set_guardian_endpoint(&endpoint, false)
                .await
                .unwrap();
            Self {
                client,
                handle,
                account_id: account.id(),
                _dir: dir,
            }
        }

        fn holding(mut self, transaction_type: TransactionType) -> Self {
            self.client
                .known_proposals
                .insert(self.account_id, PROPOSAL_ID, &transaction_type);
            self
        }

        async fn request(
            &mut self,
            request: GuardianExecutionRequest,
        ) -> crate::error::Result<guardian_client::ProposalExecution> {
            self.client
                .request_guardian_execution(PROPOSAL_ID, request)
                .await
        }

        fn guardian_was_asked(&self) -> bool {
            self.handle
                .calls()
                .iter()
                .any(|call| call == "execute_delta_proposal")
        }
    }

    fn p2id(note_type: NoteType) -> TransactionType {
        let faucet = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        TransactionType::P2ID {
            recipient: faucet,
            faucet_id: faucet,
            amount: 1,
            note_type,
            heights: P2ideHeights::default(),
        }
    }

    fn switch_guardian() -> TransactionType {
        TransactionType::SwitchGuardian {
            new_endpoint: "http://new-guardian.example".to_string(),
            new_commitment: Word::from([1u32, 2, 3, 4]),
        }
    }

    fn opted_in() -> GuardianExecutionRequest {
        GuardianExecutionRequest {
            allow_private_note: true,
        }
    }

    fn assert_local(
        outcome: crate::error::Result<guardian_client::ProposalExecution>,
        expected: LocalExecutionReason,
    ) {
        match outcome {
            Err(MultisigError::LocalExecutionRequired {
                proposal_id,
                reason,
            }) => {
                assert_eq!(proposal_id, PROPOSAL_ID);
                assert_eq!(reason, expected);
            }
            other => panic!("expected LocalExecutionRequired({expected}), got {other:?}"),
        }
    }

    #[test]
    fn the_reasons_cover_exactly_a_switch_and_a_private_p2id() {
        assert_eq!(
            LocalExecutionReason::of(&switch_guardian()),
            Some(LocalExecutionReason::SwitchGuardian)
        );
        assert_eq!(
            LocalExecutionReason::of(&p2id(NoteType::Private)),
            Some(LocalExecutionReason::PrivateNote)
        );
        assert_eq!(LocalExecutionReason::of(&p2id(NoteType::Public)), None);
        assert_eq!(
            LocalExecutionReason::of(&TransactionType::AddCosigner {
                new_commitment: Word::from([1u32, 1, 1, 1]),
            }),
            None
        );
        assert_eq!(LocalExecutionReason::of(&TransactionType::Custom), None);
        assert_eq!(
            LocalExecutionReason::SwitchGuardian.as_str(),
            "switch_guardian"
        );
        assert_eq!(LocalExecutionReason::PrivateNote.as_str(), "private_note");
        assert_eq!(
            GuardianExecutionRequest {
                allow_private_note: true,
            }
            .local_execution_reason(&switch_guardian()),
            Some(LocalExecutionReason::SwitchGuardian)
        );
        assert_eq!(
            GuardianExecutionRequest {
                allow_private_note: true,
            }
            .local_execution_reason(&p2id(NoteType::Private)),
            None
        );
    }

    #[test]
    fn the_error_says_to_execute_locally_and_names_the_reason() {
        let message = MultisigError::LocalExecutionRequired {
            proposal_id: PROPOSAL_ID.to_string(),
            reason: LocalExecutionReason::PrivateNote,
        }
        .to_string();
        assert_eq!(
            message,
            format!(
                "proposal {PROPOSAL_ID} must be executed locally, not by GUARDIAN: {}",
                LocalExecutionReason::PrivateNote.description()
            )
        );
    }

    #[tokio::test]
    async fn a_switch_is_refused_even_with_the_private_note_opt_in() {
        let mut requester = Requester::start().await.holding(switch_guardian());
        assert_local(
            requester.request(GuardianExecutionRequest::default()).await,
            LocalExecutionReason::SwitchGuardian,
        );
        assert_local(
            requester.request(opted_in()).await,
            LocalExecutionReason::SwitchGuardian,
        );
        assert!(!requester.guardian_was_asked());
    }

    #[tokio::test]
    async fn a_private_p2id_is_refused_without_the_opt_in() {
        let mut requester = Requester::start().await.holding(p2id(NoteType::Private));
        assert_local(
            requester.request(GuardianExecutionRequest::default()).await,
            LocalExecutionReason::PrivateNote,
        );
        assert!(!requester.guardian_was_asked());
    }

    #[tokio::test]
    async fn a_private_p2id_is_sent_with_the_opt_in() {
        let mut requester = Requester::start().await.holding(p2id(NoteType::Private));
        let execution = requester.request(opted_in()).await.unwrap();
        assert!(execution.newly_accepted);
        assert!(requester.guardian_was_asked());
    }

    #[tokio::test]
    async fn a_public_p2id_is_sent() {
        let mut requester = Requester::start().await.holding(p2id(NoteType::Public));
        requester
            .request(GuardianExecutionRequest::default())
            .await
            .unwrap();
        assert!(requester.guardian_was_asked());
    }

    #[tokio::test]
    async fn other_types_are_sent() {
        let mut requester =
            Requester::start()
                .await
                .holding(TransactionType::UpdateProcedureThreshold {
                    procedure: ProcedureName::ReceiveAsset,
                    new_threshold: 1,
                });
        requester
            .request(GuardianExecutionRequest::default())
            .await
            .unwrap();
        assert!(requester.guardian_was_asked());
    }

    #[tokio::test]
    async fn the_proposal_id_matches_without_its_prefix_or_case() {
        let mut requester = Requester::start().await.holding(p2id(NoteType::Public));
        requester
            .client
            .request_guardian_execution("ABC123", GuardianExecutionRequest::default())
            .await
            .unwrap();
        assert!(requester.guardian_was_asked());
    }

    #[tokio::test]
    async fn a_proposal_this_client_does_not_hold_fails_closed_without_asking_guardian() {
        let mut requester = Requester::start().await;
        match requester.request(opted_in()).await {
            Err(MultisigError::ProposalNotHeldLocally { proposal_id }) => {
                assert_eq!(proposal_id, PROPOSAL_ID)
            }
            other => panic!("expected ProposalNotHeldLocally, got {other:?}"),
        }
        assert!(!requester.guardian_was_asked());
    }

    #[tokio::test]
    async fn a_proposal_held_for_another_account_fails_closed() {
        let mut requester = Requester::start().await;
        let other = AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").unwrap();
        requester
            .client
            .known_proposals
            .insert(other, PROPOSAL_ID, &p2id(NoteType::Public));
        assert!(matches!(
            requester.request(GuardianExecutionRequest::default()).await,
            Err(MultisigError::ProposalNotHeldLocally { .. })
        ));
        assert!(!requester.guardian_was_asked());
    }
}
