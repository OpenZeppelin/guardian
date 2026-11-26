use std::sync::Arc;

use base64::Engine;
use diesel::sql_types::{BigInt, Jsonb, Text};
use diesel_async::RunQueryDsl;
use guardian_shared::{
    FromJson, ToJson,
    auth_request_payload::AuthRequestPayload,
    hex::{FromHex, IntoHex},
};
use miden_protocol::account::{
    Account, AccountCodePatch, AccountDelta, AccountStoragePatch, AccountVaultDelta,
};
use miden_protocol::block::BlockNumber;
use miden_protocol::transaction::{
    InputNotes, RawOutputNotes, TransactionLogs, TransactionSummary, TransactionSummaryUserParams,
};
use miden_protocol::{Felt, Word};

use crate::delta_object::DeltaStatus;
use crate::metadata::{
    AccountMetadata, MetadataStore, NetworkConfig, postgres::PostgresMetadataStore,
};
use crate::network::{NetworkType, miden::MidenNetworkClient};
use crate::services::{GetStateParams, get_state};
use crate::state_object::StateObject;
use crate::storage::encryption::{
    cipher::{Aes256GcmCipher, StorageCipher},
    decorator::EncryptedStorage,
    envelope::RecordAad,
    key_provider::InMemoryKeyProvider,
};
use crate::storage::{StorageBackend, postgres::PostgresService};
use crate::testing::helpers::{
    IntegrationMockNetworkClient, TestEcdsaSigner, build_guarded_multisig_account,
    create_test_app_state_with_mocks,
};

use super::*;

fn account_at_nonce(account: &Account, nonce: Felt) -> Account {
    Account::new_existing(
        account.id(),
        account.vault().clone(),
        account.storage().clone(),
        account.code().clone(),
        nonce,
    )
}

fn canonical_delta(
    network: &dyn NetworkClient,
    delta: AccountDelta,
    prev_commitment: String,
    base_state: &serde_json::Value,
) -> (DeltaObject, AppliedState) {
    let account_id = delta.id().to_hex();
    let payload = TransactionSummary::new(
        delta,
        InputNotes::new(Vec::new()).unwrap(),
        RawOutputNotes::new(Vec::new()).unwrap(),
        BlockNumber::from(0),
        Word::default(),
        0,
        TransactionSummaryUserParams::new([Felt::ZERO; 6]),
        TransactionLogs::default(),
    )
    .to_json();
    let applied = network.apply_delta(base_state, &payload).unwrap();
    let row = DeltaObject {
        account_id,
        nonce: applied.nonce.unwrap(),
        prev_commitment,
        new_commitment: Some(applied.commitment.clone()),
        delta_payload: payload,
        status: DeltaStatus::canonical("2026-11-14T12:00:00Z".into()),
        ..Default::default()
    };
    (row, applied)
}

fn canonical_history(
    account: &Account,
    network: &dyn NetworkClient,
) -> (Vec<DeltaObject>, AppliedState) {
    let (first, first_state) = canonical_delta(
        network,
        AccountDelta::try_from(account_at_nonce(account, Felt::ONE)).unwrap(),
        account.to_commitment().into_hex(),
        &serde_json::Value::Null,
    );
    let (second, second_state) = canonical_delta(
        network,
        AccountDelta::new(
            account.id(),
            AccountStoragePatch::default(),
            AccountVaultDelta::default(),
            AccountCodePatch::default(),
            Felt::ONE,
        )
        .unwrap(),
        first_state.commitment,
        &first_state.state_json,
    );
    (vec![second, first], second_state)
}

fn prove(
    network: &dyn NetworkClient,
    account_id: &str,
    target: &str,
    deltas: Vec<DeltaObject>,
) -> Result<(AppliedState, Auth), String> {
    replay(network, account_id, None, deltas)
        .and_then(|rebuilt| prove_recovery(network, account_id, target, rebuilt))
}

#[tokio::test]
async fn rejects_incomplete_history_and_commitment_mismatches() {
    let network = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
    let scheme = SignatureScheme::Ecdsa;
    let account = build_guarded_multisig_account(
        [25; 32],
        1,
        &[(Word::from([1, 2, 3, 4_u32]), scheme)],
        (Word::from([5, 6, 7, 8_u32]), scheme),
    );
    let (mut deltas, expected) = canonical_history(&account, &network);
    let account_id = account.id().to_hex();
    let target = &expected.commitment;
    assert_eq!(
        prove(&network, &account_id, target, deltas.clone())
            .unwrap()
            .0,
        expected
    );
    let first_page = replay(&network, &account_id, None, vec![deltas[1].clone()]).unwrap();
    assert_eq!(first_page.nonce, Some(1));
    assert!(prove_recovery(&network, &account_id, target, first_page.clone()).is_err());
    assert!(
        replay(
            &network,
            &account_id,
            Some(first_page.clone()),
            vec![deltas[1].clone()]
        )
        .is_err()
    );
    assert_eq!(
        replay(
            &network,
            &account_id,
            Some(first_page),
            vec![deltas[0].clone()]
        )
        .unwrap(),
        expected
    );
    assert!(prove(&network, &account_id, target, vec![deltas[0].clone()]).is_err());
    deltas[1].new_commitment = Some("wrong commitment".into());
    assert!(prove(&network, &account_id, target, deltas).is_err());
}

#[tokio::test]
async fn rebuilds_the_signer_set_under_each_scheme() {
    let network = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
    let signer = Word::from([1, 2, 3, 4_u32]);
    let commitments = vec![signer.into_hex()];
    let cases = [
        (
            SignatureScheme::Falcon,
            SignatureScheme::Falcon,
            Auth::MidenFalconRpo {
                cosigner_commitments: commitments.clone(),
            },
        ),
        (
            SignatureScheme::Ecdsa,
            SignatureScheme::Ecdsa,
            Auth::MidenEcdsa {
                cosigner_commitments: commitments.clone(),
            },
        ),
        (
            SignatureScheme::Falcon,
            SignatureScheme::Ecdsa,
            Auth::Miden {
                cosigners: vec![crate::metadata::auth::MidenCosigner {
                    commitment: commitments[0].clone(),
                    scheme: SignatureScheme::Falcon,
                }],
                guardian_scheme: SignatureScheme::Ecdsa,
            },
        ),
    ];
    for (signer_scheme, guardian_scheme, expected) in cases {
        let account = build_guarded_multisig_account(
            [26; 32],
            1,
            &[(signer, signer_scheme)],
            (Word::from([5, 6, 7, 8_u32]), guardian_scheme),
        );
        let (deltas, rebuilt) = canonical_history(&account, &network);
        let (_, auth) = prove(
            &network,
            &account.id().to_hex(),
            &rebuilt.commitment,
            deltas,
        )
        .unwrap();
        assert_eq!(auth, expected);
    }
}

#[test]
fn a_proposal_review_keeps_what_the_restored_signers_proposed_or_approved() {
    use crate::delta_object::CosignerSignature;
    use crate::metadata::auth::ChainCosigner;
    use crate::storage::ProposalRecord;
    use crate::testing::helpers::{TestSigner, create_test_delta_payload};

    let account_id = "0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b";
    let owner = TestSigner::new();
    let other_key = TestSigner::new();
    let tx_summary = create_test_delta_payload(account_id);
    let message = TransactionSummary::from_json(&tx_summary)
        .unwrap()
        .to_commitment();
    let approval = |signer: &TestSigner| CosignerSignature {
        signature: guardian_shared::ProposalSignature::Falcon {
            signature: signer.sign_word(message),
        },
        timestamp: "2026-11-14T12:50:00Z".into(),
        signer_id: signer.commitment_hex.clone(),
    };
    let payload = |approvals: &[CosignerSignature]| {
        let signatures: Vec<serde_json::Value> = approvals
            .iter()
            .map(|approval| {
                serde_json::json!({
                    "signer_id": approval.signer_id,
                    "signature": approval.signature,
                })
            })
            .collect();
        serde_json::json!({ "tx_summary": tx_summary, "signatures": signatures })
    };
    let pending = |proposer: &TestSigner, approvals: &[CosignerSignature]| DeltaStatus::Pending {
        timestamp: "2026-11-14T12:45:00Z".into(),
        proposer_id: proposer.commitment_hex.clone(),
        cosigner_sigs: approvals.to_vec(),
    };
    let record =
        |commitment: &str, proposer: &TestSigner, approvals: &[CosignerSignature]| ProposalRecord {
            account_id: account_id.into(),
            commitment: commitment.into(),
            proposal: DeltaObject {
                account_id: account_id.into(),
                delta_payload: payload(approvals),
                status: pending(proposer, approvals),
                ..Default::default()
            },
        };
    let restored = |scheme: SignatureScheme| AppliedState {
        state_json: serde_json::Value::Null,
        commitment: "0xrestored".into(),
        nonce: Some(1),
        cosigners: vec![ChainCosigner {
            commitment: owner.commitment_hex.clone(),
            scheme: Some(scheme),
        }],
        guardian_commitment: None,
    };
    let own = approval(&owner);
    let other = approval(&other_key);
    let mut unreadable = record("unreadable", &other_key, std::slice::from_ref(&other));
    unreadable.proposal.delta_payload = serde_json::json!({ "tx_summary": "not a summary" });
    let records = [
        record("signed", &owner, std::slice::from_ref(&own)),
        record("mixed", &owner, &[own.clone(), other.clone()]),
        record("approved", &other_key, std::slice::from_ref(&own)),
        record("unrelated", &other_key, std::slice::from_ref(&other)),
        record("unsigned", &owner, &[]),
        unreadable,
    ];

    let changes: Vec<ProposalChange> =
        proposals::review(&restored(SignatureScheme::Falcon), &records)
            .into_iter()
            .map(|reviewed| reviewed.change)
            .collect();
    assert_eq!(
        changes,
        [
            ProposalChange::Keep,
            ProposalChange::DropApprovals {
                status: pending(&owner, std::slice::from_ref(&own)),
                delta_payload: payload(std::slice::from_ref(&own)),
            },
            ProposalChange::Keep,
            ProposalChange::Remove,
            ProposalChange::Keep,
            ProposalChange::Keep,
        ]
    );

    let under_another_scheme = proposals::review(&restored(SignatureScheme::Ecdsa), &records[..1]);
    assert_eq!(
        under_another_scheme[0].change,
        ProposalChange::DropApprovals {
            status: pending(&owner, &[]),
            delta_payload: payload(&[]),
        }
    );
}

#[tokio::test]
async fn partial_history_fixture_cannot_rebuild_a_missing_base() {
    let network = MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal);
    let delta: DeltaObject = serde_json::from_str(crate::testing::fixtures::DELTA_1_JSON).unwrap();
    let summary = TransactionSummary::from_json(&delta.delta_payload).unwrap();
    assert!(summary.account_delta().code().is_empty());
    assert!(
        network
            .apply_delta(&serde_json::Value::Null, &delta.delta_payload)
            .is_err()
    );
}

#[tokio::test]
#[ignore = "requires a dedicated Postgres test database"]
async fn account_recovery_restores_verified_state_once_and_preserves_encrypted_evidence() {
    let url = crate::testing::pg::test_database_url().await;
    let raw = Arc::new(PostgresService::new(&url, 4).await.unwrap());
    let metadata = Arc::new(PostgresMetadataStore::new(&url, 4).await.unwrap());
    let key = base64::engine::general_purpose::STANDARD.encode([2_u8; 32]);
    let provider = InMemoryKeyProvider::from_dev_key(&key, "recovery-test").unwrap();
    let cipher = Arc::new(Aes256GcmCipher::new(Arc::new(provider)));
    let storage = Arc::new(EncryptedStorage::new(raw, cipher.clone()));
    let store = RecoveryStore::new(
        metadata.pool_handle(),
        Some(cipher.clone()),
        "test-recovery",
    );
    let contender = RecoveryStore::new(
        metadata.pool_handle(),
        Some(cipher.clone()),
        "other-recovery",
    );
    let lease = store
        .leader
        .try_acquire(Duration::from_secs(300))
        .await
        .unwrap()
        .unwrap();
    let chain = Arc::new(IntegrationMockNetworkClient::new(
        MidenNetworkClient::lazy_for_test(NetworkType::MidenLocal),
    ));
    let state = create_test_app_state_with_mocks(storage.clone(), chain.clone(), metadata.clone());
    let scheme = SignatureScheme::Ecdsa;
    let signer = TestEcdsaSigner::new();
    let account = build_guarded_multisig_account(
        [25; 32],
        1,
        &[(Word::from_hex(&signer.commitment_hex).unwrap(), scheme)],
        (
            Word::from_hex(&state.ack.commitment(&scheme)).unwrap(),
            scheme,
        ),
    );
    let (deltas, canonical) = canonical_history(&account, chain.as_ref());
    let account_id = &deltas[0].account_id;
    let target = canonical.commitment.as_str();
    let old_auth = Auth::MidenEcdsa {
        cosigner_commitments: vec![Word::from([8, 7, 6, 5_u32]).into_hex()],
    };
    let expected_auth = Auth::MidenEcdsa {
        cosigner_commitments: vec![signer.commitment_hex.clone()],
    };
    let other_state = account_at_nonce(&account, Felt::from(2_u8));
    let prior = StateObject {
        account_id: account_id.clone(),
        state_json: other_state.to_json(),
        commitment: other_state.to_commitment().into_hex(),
        nonce: Some(2),
        created_at: "2026-11-14T12:00:00Z".into(),
        updated_at: "2026-11-14T13:00:00Z".into(),
        auth_scheme: "ecdsa".into(),
    };
    metadata
        .set(AccountMetadata {
            account_id: account_id.clone(),
            auth: old_auth.clone(),
            network_config: NetworkConfig::miden_default(),
            created_at: prior.created_at.clone(),
            updated_at: prior.updated_at.clone(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        })
        .await
        .unwrap();
    storage
        .submit_state(&StateObject {
            state_json: canonical.state_json.clone(),
            commitment: canonical.commitment.clone(),
            updated_at: prior.created_at.clone(),
            ..prior.clone()
        })
        .await
        .unwrap();
    storage.replace_state(&prior, target).await.unwrap();
    for delta in &deltas {
        storage.submit_delta(delta).await.unwrap();
    }
    let testnet = NetworkConfig::Miden {
        network_type: MidenNetworkType::Testnet,
    };
    {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        diesel::sql_query("INSERT INTO account_recovery_startup (network_config) VALUES ($1)")
            .bind::<Jsonb, _>(serde_json::to_value(&testnet).unwrap())
            .execute(&mut conn)
            .await
            .unwrap();
        for label in [&testnet, &NetworkConfig::miden_default()] {
            diesel::sql_query(
                "UPDATE account_metadata SET network_config = $2 WHERE account_id = $1",
            )
            .bind::<Text, _>(account_id)
            .bind::<Jsonb, _>(serde_json::to_value(label).unwrap())
            .execute(&mut conn)
            .await
            .unwrap();
            let candidates = store
                .candidates(MidenNetworkType::Mainnet, "", 10)
                .await
                .unwrap();
            assert_eq!(candidates.len(), usize::from(label != &testnet));
        }
    }
    assert_eq!(
        storage.pull_state_head(account_id).await.unwrap().nonce,
        None
    );
    assert_eq!(
        metadata
            .get_for_read(account_id)
            .await
            .unwrap()
            .unwrap()
            .auth,
        old_auth
    );
    let mut candidates = store
        .candidates(MidenNetworkType::Testnet, "", 10)
        .await
        .unwrap();
    assert_eq!(candidates.len(), 1);
    let candidate = candidates.pop().unwrap();
    assert_eq!(candidate.canonical_nonce, Some(2));
    assert_eq!(candidate.stored_nonce, None);

    let mut absent = state.clone();
    absent.network_client = Arc::new(
        crate::testing::mocks::MockNetworkClient::new()
            .with_verify_commitment(Ok(StateVerification::Absent))
            .with_verify_commitment(Ok(StateVerification::Absent)),
    );
    assert!(matches!(
        check_candidate(&absent, &store, &candidate).await.unwrap(),
        RecoveryOutcome::Retryable(reason) if reason.contains("no record of the account")
    ));
    let mut undeployed = store
        .candidates(MidenNetworkType::Local, "", 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    undeployed.canonical_nonce = None;
    undeployed.canonical_commitment = None;
    let absent_with = |first_state: Result<(), String>| {
        let mut absent = state.clone();
        absent.network_client = Arc::new(
            crate::testing::mocks::MockNetworkClient::new()
                .with_verify_commitment(Ok(StateVerification::Absent))
                .with_validate_first_state(first_state),
        );
        absent
    };
    for stored_nonce in [Some(0), None] {
        undeployed.stored_nonce = stored_nonce;
        assert!(matches!(
            check_candidate(&absent_with(Ok(())), &store, &undeployed)
                .await
                .unwrap(),
            RecoveryOutcome::Unchanged
        ));
        assert!(matches!(
            check_candidate(&absent_with(Err("nonce is 5, not 0".into())), &store, &undeployed)
                .await
                .unwrap(),
            RecoveryOutcome::Unresolved(reason) if reason.contains("no record of the account")
        ));
    }
    undeployed.stored_nonce = Some(5);
    assert!(matches!(
        check_candidate(&absent_with(Ok(())), &store, &undeployed)
            .await
            .unwrap(),
        RecoveryOutcome::Unresolved(reason) if reason.contains("no record of the account")
    ));

    {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        diesel::sql_query("BEGIN").execute(&mut conn).await.unwrap();
        diesel::sql_query("LOCK TABLE states IN ACCESS EXCLUSIVE MODE")
            .execute(&mut conn)
            .await
            .unwrap();
        let timed_out = run(
            &state,
            &store,
            MidenNetworkType::Testnet,
            ScanWrites::Apply(&lease),
            &CancellationToken::new(),
            "",
            Instant::now() + Duration::from_millis(40),
        )
        .await
        .unwrap();
        diesel::sql_query("ROLLBACK")
            .execute(&mut conn)
            .await
            .unwrap();
        assert!(timed_out.stopped_early);
        assert_eq!(timed_out.checked, 0);
    }

    for page in [1, 2, HISTORY_PAGE] {
        assert!(matches!(
            rebuild(&state, account_id, 2, page).await.unwrap(),
            Rebuild::Rebuilt(rebuilt) if rebuilt.commitment == target && rebuilt.nonce == Some(2)
        ));
    }
    assert!(matches!(
        rebuild(&state, account_id, 3, 1).await.unwrap(),
        Rebuild::Unresolved(reason) if reason.contains("not contiguous")
    ));

    chain.set_on_chain_commitment(account_id, "different head");
    assert!(matches!(
        check_candidate(&state, &store, &candidate).await.unwrap(),
        RecoveryOutcome::Unresolved(reason) if reason.contains("missing from the canonical history")
    ));
    assert_ne!(deltas[1].prev_commitment, prior.commitment);
    for earlier in [
        deltas[1].new_commitment.as_deref().unwrap(),
        deltas[1].prev_commitment.as_str(),
    ] {
        chain.set_on_chain_commitment(account_id, earlier);
        assert!(matches!(
            check_candidate(&state, &store, &candidate).await.unwrap(),
            RecoveryOutcome::Retryable(reason) if reason.contains("earlier state")
        ));
    }
    chain.set_on_chain_commitment(account_id, target);
    let pending = DeltaObject {
        nonce: 3,
        prev_commitment: target.into(),
        new_commitment: None,
        status: DeltaStatus::candidate("2026-11-14T12:30:00Z".into()),
        ..deltas[0].clone()
    };
    storage.submit_delta(&pending).await.unwrap();
    let current_candidate = || async {
        store
            .candidates(MidenNetworkType::Testnet, "", 10)
            .await
            .unwrap()
            .pop()
            .unwrap()
    };
    assert_eq!(
        current_candidate().await.unsettled_work.as_deref(),
        Some("a delta awaits its on-chain outcome")
    );
    assert!(matches!(
        check_candidate(&state, &store, &candidate).await.unwrap(),
        RecoveryOutcome::Verified(_)
    ));
    assert!(
        !store
            .apply(
                &candidate,
                &canonical.state_json,
                &expected_auth,
                &ProposalReview::Reviewed(Vec::new()),
                state.clock.now(),
                &lease
            )
            .await
            .unwrap()
    );
    assert_eq!(
        storage.pull_state(account_id).await.unwrap().commitment,
        prior.commitment
    );
    assert!(matches!(
        run_within(
            &state,
            &contender,
            MidenNetworkType::Testnet,
            SCAN_BUDGET,
            Duration::ZERO
        )
        .await,
        Err(StartupRecoveryError::LeaseBusy)
    ));
    store.leader.release(lease.clone()).await.unwrap();

    run_once(&state, &store, MidenNetworkType::Testnet)
        .await
        .unwrap();
    assert!(
        !store
            .startup_completed(MidenNetworkType::Testnet)
            .await
            .unwrap()
    );

    storage
        .update_delta_status(
            account_id,
            3,
            DeltaStatus::discarded_client_abandoned("2026-11-14T12:40:00Z".into()),
        )
        .await
        .unwrap();
    storage
        .submit_delta(&DeltaObject {
            nonce: 4,
            prev_commitment: prior.commitment.clone(),
            new_commitment: None,
            status: DeltaStatus::candidate("2026-11-14T12:41:00Z".into()),
            ..deltas[0].clone()
        })
        .await
        .unwrap();
    assert_eq!(current_candidate().await.unsettled_work, None);
    {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        diesel::sql_query(
            "INSERT INTO execution_reservations
                 (account_id, proposal_id, attempt, holder_id, lease_name, fence_token,
                  lease_expires_at, phase)
             VALUES ($1, '0xreserved', 1, 'holder', 'execution', 1, now(), 'accepted')",
        )
        .bind::<Text, _>(account_id)
        .execute(&mut conn)
        .await
        .unwrap();
        diesel::sql_query(
            "INSERT INTO execution_submissions
                 (account_id, proposal_id, attempt, candidate_nonce, transaction_id,
                  expected_commitment, reference_block, expiration_block, base_commitment)
             VALUES ($1, '0xreserved', 1, 3, '0xtx', '0xexpected', 1, 2, $2)",
        )
        .bind::<Text, _>(account_id)
        .bind::<Text, _>(target)
        .execute(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            current_candidate().await.unsettled_work.as_deref(),
            Some("an execution is reserved")
        );
        diesel::sql_query(
            "UPDATE execution_submissions SET base_commitment = $2 WHERE account_id = $1",
        )
        .bind::<Text, _>(account_id)
        .bind::<Text, _>(&prior.commitment)
        .execute(&mut conn)
        .await
        .unwrap();
        assert_eq!(current_candidate().await.unsettled_work, None);

        diesel::sql_query(
            "UPDATE deltas SET new_commitment = '0xlanded' WHERE account_id = $1 AND nonce = 4",
        )
        .bind::<Text, _>(account_id)
        .execute(&mut conn)
        .await
        .unwrap();
        chain.set_on_chain_commitment(account_id, "0xlanded");
        assert!(matches!(
            check_candidate(&state, &store, &current_candidate().await)
                .await
                .unwrap(),
            RecoveryOutcome::Retryable(reason) if reason.contains("landed")
        ));
        chain.set_on_chain_commitment(account_id, target);
    }

    assert!(
        store
            .complete_startup(MidenNetworkType::Testnet, 0, &lease)
            .await
            .is_err()
    );

    let mut unavailable = state.clone();
    unavailable.network_client = Arc::new(
        crate::testing::mocks::MockNetworkClient::new()
            .with_verify_commitment(Err("simulated RPC outage".into())),
    );
    let mut conn = metadata.pool_handle().get().await.unwrap();
    diesel::sql_query("DELETE FROM account_recovery_pending WHERE account_id = $1")
        .bind::<Text, _>(account_id)
        .execute(&mut conn)
        .await
        .unwrap();
    run_once(&unavailable, &store, MidenNetworkType::Testnet)
        .await
        .unwrap();
    assert!(
        !store
            .startup_completed(MidenNetworkType::Testnet)
            .await
            .unwrap()
    );

    #[derive(diesel::QueryableByName)]
    struct DeferredReason {
        #[diesel(sql_type = Text)]
        reason: String,
    }
    let deferred = diesel::sql_query(
        "SELECT reason FROM account_recovery_pending WHERE account_id = $1 AND outcome = 'deferred'",
    )
    .bind::<Text, _>(account_id)
    .get_result::<DeferredReason>(&mut conn)
    .await
    .unwrap();
    assert!(deferred.reason.contains("account verification failed"));

    diesel::sql_query("BEGIN").execute(&mut conn).await.unwrap();
    diesel::sql_query("LOCK TABLE account_recovery_backups IN ACCESS EXCLUSIVE MODE")
        .execute(&mut conn)
        .await
        .unwrap();
    let blocked_restore = timeout(
        Duration::from_secs(8),
        run_once(&state, &store, MidenNetworkType::Testnet),
    )
    .await;
    diesel::sql_query("ROLLBACK")
        .execute(&mut conn)
        .await
        .unwrap();
    blocked_restore.unwrap().unwrap();
    assert_eq!(
        storage.pull_state(account_id).await.unwrap().commitment,
        prior.commitment
    );
    let deferred = diesel::sql_query(
        "SELECT reason FROM account_recovery_pending WHERE account_id = $1 AND outcome = 'deferred'",
    )
    .bind::<Text, _>(account_id)
    .get_result::<DeferredReason>(&mut conn)
    .await
    .unwrap();
    assert!(deferred.reason.contains("restore transaction failed"));
    drop(conn);

    let other_key = crate::testing::helpers::TestSigner::new();
    let tx_summary = crate::testing::helpers::create_test_delta_payload(account_id);
    let message = TransactionSummary::from_json(&tx_summary)
        .unwrap()
        .to_commitment();
    let approval = |signature: guardian_shared::ProposalSignature, signer_id: &str| {
        crate::delta_object::CosignerSignature {
            signature,
            timestamp: "2026-11-14T12:50:00Z".into(),
            signer_id: signer_id.into(),
        }
    };
    let other_approval = approval(
        guardian_shared::ProposalSignature::Falcon {
            signature: other_key.sign_word(message),
        },
        &other_key.commitment_hex,
    );
    let signer_approval = approval(
        guardian_shared::ProposalSignature::Ecdsa {
            signature: signer.sign_word(message),
            public_key: Some(signer.pubkey_hex.clone()),
            message_format: guardian_shared::EcdsaMessageFormat::Raw,
        },
        &signer.commitment_hex,
    );
    let pending_proposal =
        |nonce: u64, proposer: &str, approvals: &[crate::delta_object::CosignerSignature]| {
            let signatures: Vec<serde_json::Value> = approvals
                .iter()
                .map(|approval| {
                    serde_json::json!({
                        "signer_id": approval.signer_id,
                        "signature": approval.signature,
                    })
                })
                .collect();
            DeltaObject {
                account_id: account_id.clone(),
                nonce,
                prev_commitment: prior.commitment.clone(),
                delta_payload: serde_json::json!({
                    "tx_summary": tx_summary,
                    "signatures": signatures,
                }),
                status: DeltaStatus::Pending {
                    timestamp: "2026-11-14T12:45:00Z".into(),
                    proposer_id: proposer.into(),
                    cosigner_sigs: approvals.to_vec(),
                },
                ..Default::default()
            }
        };
    let untouched = pending_proposal(
        6,
        &signer.commitment_hex,
        std::slice::from_ref(&signer_approval),
    );
    for (commitment, proposal) in [
        (
            "0xproposed-by-another-key",
            pending_proposal(
                4,
                &other_key.commitment_hex,
                std::slice::from_ref(&other_approval),
            ),
        ),
        (
            "0xproposed-by-the-signer",
            pending_proposal(
                5,
                &signer.commitment_hex,
                &[signer_approval.clone(), other_approval.clone()],
            ),
        ),
        ("0xsigned-by-the-signer-only", untouched.clone()),
    ] {
        storage
            .submit_delta_proposal(commitment, &proposal)
            .await
            .unwrap();
    }

    #[derive(diesel::QueryableByName, Debug, PartialEq, Eq)]
    struct RecoveryRows {
        #[diesel(sql_type = BigInt)]
        pending: i64,
        #[diesel(sql_type = BigInt)]
        backups: i64,
        #[diesel(sql_type = Text)]
        progress: String,
    }
    let recovery_rows = || async {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        diesel::sql_query(
            "SELECT (SELECT count(*) FROM account_recovery_pending) AS pending,
                    (SELECT count(*) FROM account_recovery_backups) AS backups,
                    (SELECT COALESCE(string_agg(to_jsonb(s)::text, ',' ORDER BY s.network_config::text), '')
                       FROM account_recovery_startup s) AS progress",
        )
        .get_result::<RecoveryRows>(&mut conn)
        .await
        .unwrap()
    };
    let before_dry_run = recovery_rows().await;
    assert_eq!((before_dry_run.pending, before_dry_run.backups), (1, 0));
    dry_run(&state, &store, MidenNetworkType::Testnet)
        .await
        .unwrap();
    assert_eq!(recovery_rows().await, before_dry_run);
    assert_eq!(
        storage.pull_state(account_id).await.unwrap().commitment,
        prior.commitment
    );
    assert_eq!(
        storage
            .pull_pending_proposals(account_id)
            .await
            .unwrap()
            .len(),
        3
    );

    let reviewing = store
        .leader
        .try_acquire(Duration::from_secs(300))
        .await
        .unwrap()
        .unwrap();
    let reviewed_candidate = store
        .candidates(MidenNetworkType::Testnet, "", 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let RecoveryOutcome::Verified(reviewed) = check_candidate(&state, &store, &reviewed_candidate)
        .await
        .unwrap()
    else {
        panic!("the account is restorable");
    };
    assert_eq!(reviewed.changed_proposals().count(), 2);
    storage
        .submit_delta_proposal(
            "0xadded-after-the-review",
            &pending_proposal(7, &signer.commitment_hex, &[]),
        )
        .await
        .unwrap();
    assert!(
        !store
            .apply(
                &reviewed_candidate,
                &reviewed.state.state_json,
                &reviewed.auth,
                &reviewed.proposals,
                state.clock.now(),
                &reviewing,
            )
            .await
            .unwrap()
    );
    assert_eq!(
        storage.pull_state(account_id).await.unwrap().commitment,
        prior.commitment
    );
    storage
        .delete_delta_proposal(account_id, "0xadded-after-the-review")
        .await
        .unwrap();
    store.leader.release(reviewing).await.unwrap();
    metadata
        .set_pause(account_id, state.clock.now(), "operator review")
        .await
        .unwrap();
    {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        diesel::sql_query("UPDATE states SET nonce = 9 WHERE account_id = $1")
            .bind::<Text, _>(account_id)
            .execute(&mut conn)
            .await
            .unwrap();
    }
    assert_eq!(
        storage.pull_state_head(account_id).await.unwrap().nonce,
        Some(9)
    );
    let eligible = store
        .candidates(MidenNetworkType::Testnet, "", 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        !store
            .apply(
                &eligible,
                &reviewed.state.state_json,
                &reviewed.auth,
                &reviewed.proposals,
                state.clock.now(),
                &lease,
            )
            .await
            .unwrap()
    );
    assert_eq!(
        storage.pull_state(account_id).await.unwrap().commitment,
        prior.commitment
    );
    {
        #[derive(diesel::QueryableByName)]
        struct Base {
            #[diesel(sql_type = Text)]
            prev_commitment: String,
        }
        let reserved_proposal = "0xproposed-by-the-signer";
        let rebase = |prev_commitment: String| {
            let metadata = metadata.clone();
            async move {
                let mut conn = metadata.pool_handle().get().await.unwrap();
                diesel::sql_query(
                    "UPDATE delta_proposals SET prev_commitment = $3
                     WHERE account_id = $1 AND commitment = $2",
                )
                .bind::<Text, _>(account_id)
                .bind::<Text, _>(reserved_proposal)
                .bind::<Text, _>(prev_commitment)
                .execute(&mut conn)
                .await
                .unwrap()
            }
        };
        let original = {
            let mut conn = metadata.pool_handle().get().await.unwrap();
            diesel::sql_query("DELETE FROM execution_submissions WHERE account_id = $1")
                .bind::<Text, _>(account_id)
                .execute(&mut conn)
                .await
                .unwrap();
            diesel::sql_query(
                "UPDATE execution_reservations SET proposal_id = $2 WHERE account_id = $1",
            )
            .bind::<Text, _>(account_id)
            .bind::<Text, _>(reserved_proposal)
            .execute(&mut conn)
            .await
            .unwrap();
            diesel::sql_query(
                "SELECT prev_commitment FROM delta_proposals
                 WHERE account_id = $1 AND commitment = $2",
            )
            .bind::<Text, _>(account_id)
            .bind::<Text, _>(reserved_proposal)
            .get_result::<Base>(&mut conn)
            .await
            .unwrap()
            .prev_commitment
        };
        assert_ne!(original, target);
        assert_eq!(current_candidate().await.unsettled_work, None);

        assert_eq!(rebase(target.to_owned()).await, 1);
        assert_eq!(
            current_candidate().await.unsettled_work.as_deref(),
            Some("an execution is reserved")
        );
        let current = store
            .leader
            .try_acquire(Duration::from_secs(300))
            .await
            .unwrap()
            .unwrap();
        assert!(
            !store
                .apply(
                    &eligible,
                    &reviewed.state.state_json,
                    &reviewed.auth,
                    &reviewed.proposals,
                    state.clock.now(),
                    &current,
                )
                .await
                .unwrap()
        );
        store.leader.release(current).await.unwrap();
        assert_eq!(rebase(original).await, 1);
        assert_eq!(current_candidate().await.unsettled_work, None);
    }
    storage
        .submit_delta_proposal(
            "0xunreadable-status",
            &pending_proposal(8, &other_key.commitment_hex, &[]),
        )
        .await
        .unwrap();
    {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        let updated = diesel::sql_query(
            "UPDATE delta_proposals SET status = '{\"status\": \"pending\", \"cosigner_sigs\": 1}'
             WHERE account_id = $1 AND commitment = '0xunreadable-status'",
        )
        .bind::<Text, _>(account_id)
        .execute(&mut conn)
        .await
        .unwrap();
        assert_eq!(updated, 1);
    }

    run_once(&state, &store, MidenNetworkType::Testnet)
        .await
        .unwrap();
    assert!(
        store
            .startup_completed(MidenNetworkType::Testnet)
            .await
            .unwrap()
    );
    assert!(
        !store
            .startup_completed(MidenNetworkType::Local)
            .await
            .unwrap()
    );
    assert_eq!(
        storage.pull_state_head(account_id).await.unwrap().nonce,
        Some(2)
    );

    let kept = storage.pull_pending_proposals(account_id).await.unwrap();
    assert_eq!(
        kept.iter()
            .map(|record| record.commitment.as_str())
            .collect::<Vec<_>>(),
        ["0xproposed-by-the-signer", "0xsigned-by-the-signer-only"]
    );
    {
        #[derive(diesel::QueryableByName)]
        struct Rows {
            #[diesel(sql_type = BigInt)]
            total: i64,
        }
        let mut conn = metadata.pool_handle().get().await.unwrap();
        let unreadable = diesel::sql_query(
            "SELECT count(*) AS total FROM delta_proposals
             WHERE account_id = $1 AND commitment = '0xunreadable-status'",
        )
        .bind::<Text, _>(account_id)
        .get_result::<Rows>(&mut conn)
        .await
        .unwrap();
        assert_eq!(unreadable.total, 1);
    }
    let trimmed = pending_proposal(
        5,
        &signer.commitment_hex,
        std::slice::from_ref(&signer_approval),
    );
    assert_eq!(
        (&kept[0].proposal.status, &kept[0].proposal.delta_payload),
        (&trimmed.status, &trimmed.delta_payload)
    );
    assert_eq!(
        (&kept[1].proposal.status, &kept[1].proposal.delta_payload),
        (&untouched.status, &untouched.delta_payload)
    );

    let held = contender
        .leader
        .try_acquire(LEASE_TTL)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        run_within(
            &state,
            &store,
            MidenNetworkType::Testnet,
            SCAN_BUDGET,
            Duration::ZERO
        )
        .await,
        Err(StartupRecoveryError::LeaseBusy)
    ));
    assert!(contender.leader.verify_held(&held).await.unwrap());
    contender.leader.release(held).await.unwrap();
    run_once(&state, &store, MidenNetworkType::Testnet)
        .await
        .unwrap();
    assert!(
        !store
            .apply(
                &candidate,
                &canonical.state_json,
                &expected_auth,
                &ProposalReview::Reviewed(Vec::new()),
                state.clock.now(),
                &lease
            )
            .await
            .unwrap()
    );
    let payload =
        AuthRequestPayload::from_json_serializable(&serde_json::json!({"account_id": account_id}))
            .unwrap();
    let (signature, timestamp) = signer.sign_request(account_id, &payload);
    let restored = get_state(
        &state,
        GetStateParams {
            account_id: account_id.clone(),
            credentials: crate::metadata::Credentials::signature(
                signer.pubkey_hex,
                signature,
                timestamp,
            )
            .with_request_payload(payload),
        },
    )
    .await
    .unwrap()
    .state;
    assert_eq!(
        chain
            .get_state_head(account_id, &restored.state_json)
            .unwrap()
            .commitment,
        target
    );
    let restored_metadata = metadata.get_for_read(account_id).await.unwrap().unwrap();
    assert_eq!(restored_metadata.auth, expected_auth);
    assert!(restored_metadata.paused_at.is_some());
    assert_eq!(
        storage.pull_state_head(account_id).await.unwrap().nonce,
        Some(2)
    );
    assert_eq!(
        restored_metadata.paused_reason.as_deref(),
        Some("operator review")
    );

    #[derive(diesel::QueryableByName)]
    struct Backup {
        #[diesel(sql_type = BigInt)]
        total: i64,
        #[diesel(sql_type = Jsonb)]
        prior_state: serde_json::Value,
        #[diesel(sql_type = Jsonb)]
        prior_auth: serde_json::Value,
        #[diesel(sql_type = Jsonb)]
        prior_proposals: serde_json::Value,
    }
    let mut conn = metadata.pool_handle().get().await.unwrap();
    let backup = diesel::sql_query("SELECT count(*) OVER () AS total, prior_state, prior_auth, prior_proposals FROM account_recovery_backups WHERE account_id = $1")
        .bind::<Text, _>(account_id).get_result::<Backup>(&mut conn).await.unwrap();
    let old_payload = &backup.prior_state["state_json"];
    assert_ne!(old_payload, &prior.state_json);
    assert_eq!(
        cipher
            .decrypt(&RecordAad::State { account_id }, old_payload)
            .unwrap(),
        prior.state_json
    );
    assert_eq!(
        serde_json::from_value::<Auth>(backup.prior_auth).unwrap(),
        old_auth
    );
    assert_eq!(backup.total, 1);
    let saved_proposals: Vec<&str> = backup
        .prior_proposals
        .as_array()
        .unwrap()
        .iter()
        .map(|proposal| proposal["commitment"].as_str().unwrap())
        .collect();
    assert_eq!(
        saved_proposals,
        ["0xproposed-by-another-key", "0xproposed-by-the-signer"]
    );

    #[derive(diesel::QueryableByName)]
    struct PendingCount {
        #[diesel(sql_type = BigInt)]
        pending: i64,
    }
    let pending = diesel::sql_query(
        "SELECT count(*) AS pending FROM account_recovery_pending WHERE account_id = $1",
    )
    .bind::<Text, _>(account_id)
    .get_result::<PendingCount>(&mut conn)
    .await
    .unwrap();
    assert_eq!(pending.pending, 0);
}

fn scan_progress(resume_after: &str) -> ScanProgress {
    ScanProgress {
        resume_after: resume_after.into(),
        ..Default::default()
    }
}

#[test]
fn a_scan_can_complete_across_startups_when_the_pass_deferred_no_account() {
    let interrupted = ScanSummary {
        unresolved: 2,
        last_checked: Some("0xb".into()),
        stopped_early: true,
        ..Default::default()
    };
    assert_eq!(
        interrupted.next_step(scan_progress("0xa")),
        ScanStep::Resume(ScanProgress {
            unresolved: 2,
            ..scan_progress("0xb")
        })
    );

    let stalled = ScanSummary {
        stopped_early: true,
        ..Default::default()
    };
    assert_eq!(
        stalled.next_step(scan_progress("0xa")),
        ScanStep::Resume(scan_progress("0xa"))
    );

    let finished = ScanSummary {
        unresolved: 1,
        last_checked: Some("0xc".into()),
        ..Default::default()
    };
    assert_eq!(
        finished.next_step(ScanProgress::default()),
        ScanStep::Completed { unresolved: 1 }
    );
    assert_eq!(
        finished.next_step(ScanProgress {
            unresolved: 2,
            ..scan_progress("0xa")
        }),
        ScanStep::Completed { unresolved: 3 }
    );
    assert_eq!(
        finished.next_step(ScanProgress {
            deferred: true,
            ..scan_progress("0xa")
        }),
        ScanStep::Resume(ScanProgress::default())
    );

    let rolled_back = ScanSummary {
        failed: 3,
        failed_ahead_of_position: 3,
        last_checked: Some("0xa".into()),
        stopped_early: true,
        ..Default::default()
    };
    assert_eq!(
        rolled_back.next_step(ScanProgress::default()),
        ScanStep::Resume(scan_progress("0xa"))
    );
    let failed_behind_position = ScanSummary {
        failed: 4,
        ..rolled_back
    };
    assert_eq!(
        failed_behind_position.next_step(ScanProgress::default()),
        ScanStep::Resume(ScanProgress {
            deferred: true,
            ..scan_progress("0xa")
        })
    );

    for deferring in [
        ScanSummary {
            retryable: 1,
            ..Default::default()
        },
        ScanSummary {
            failed: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            deferring.next_step(ScanProgress::default()),
            ScanStep::Resume(ScanProgress::default())
        );
    }
}

#[tokio::test]
#[ignore = "requires a dedicated Postgres test database"]
async fn account_recovery_scan_continues_from_its_saved_position_and_records_pending_accounts() {
    let url = crate::testing::pg::test_database_url().await;
    let storage = Arc::new(PostgresService::new(&url, 4).await.unwrap());
    let metadata = Arc::new(PostgresMetadataStore::new(&url, 4).await.unwrap());
    let store = RecoveryStore::new(metadata.pool_handle(), None, "resume-recovery");
    let network = MidenNetworkType::Cedar;
    let first = "0xresume0000000000000000000000a";
    let second = "0xresume0000000000000000000000b";
    let joined_later = "0xresume0000000000000000000000c";
    for account_id in [first, second, joined_later] {
        metadata
            .set(AccountMetadata {
                account_id: account_id.into(),
                auth: Auth::MidenEcdsa {
                    cosigner_commitments: vec![Word::from([1, 2, 3, 4_u32]).into_hex()],
                },
                network_config: NetworkConfig::Miden {
                    network_type: network,
                },
                created_at: "2026-11-14T12:00:00Z".into(),
                updated_at: "2026-11-14T12:00:00Z".into(),
                has_pending_candidate: false,
                paused_at: None,
                paused_reason: None,
                released_at: None,
            })
            .await
            .unwrap();
        storage
            .submit_state(&StateObject {
                account_id: account_id.into(),
                state_json: serde_json::json!({}),
                commitment: format!("{account_id}-stored"),
                nonce: Some(1),
                created_at: "2026-11-14T12:00:00Z".into(),
                updated_at: "2026-11-14T13:00:00Z".into(),
                auth_scheme: "ecdsa".into(),
            })
            .await
            .unwrap();
    }
    for (nonce, prev_commitment, new_commitment) in [
        (3, "0xheld-base", "0xheld-middle"),
        (4, "0xheld-middle", "0xheld-head"),
    ] {
        storage
            .submit_delta(&DeltaObject {
                account_id: joined_later.into(),
                nonce,
                prev_commitment: prev_commitment.into(),
                new_commitment: Some(new_commitment.into()),
                delta_payload: serde_json::json!({}),
                status: DeltaStatus::canonical("2026-11-14T12:30:00Z".into()),
                ..Default::default()
            })
            .await
            .unwrap();
    }
    let chain_head = |account_id: &str| StateVerification::Mismatch {
        on_chain: if account_id == joined_later {
            "0xheld-head".into()
        } else {
            "0xelsewhere".into()
        },
    };
    let checking = |accounts: &[&str]| {
        let chain = accounts.iter().rev().fold(
            crate::testing::mocks::MockNetworkClient::new(),
            |chain, account_id| chain.with_verify_commitment(Ok(chain_head(account_id))),
        );
        create_test_app_state_with_mocks(storage.clone(), Arc::new(chain), metadata.clone())
    };
    let lease = store
        .leader
        .try_acquire(Duration::from_secs(300))
        .await
        .unwrap()
        .unwrap();
    let later = || Instant::now() + Duration::from_secs(60);

    let whole = run(
        &checking(&[first, second, joined_later]),
        &store,
        network,
        ScanWrites::Apply(&lease),
        &CancellationToken::new(),
        "",
        later(),
    )
    .await
    .unwrap();
    assert_eq!((whole.checked, whole.unresolved), (3, 3));
    assert_eq!(whole.last_checked.as_deref(), Some(joined_later));
    assert!(!whole.stopped_early);

    #[derive(diesel::QueryableByName, Debug, PartialEq, Eq)]
    struct Pending {
        #[diesel(sql_type = Text)]
        account_id: String,
        #[diesel(sql_type = Text)]
        outcome: String,
        #[diesel(sql_type = Text)]
        reason: String,
        #[diesel(sql_type = diesel::sql_types::Nullable<BigInt>)]
        canonical_nonce: Option<i64>,
        #[diesel(sql_type = diesel::sql_types::Nullable<BigInt>)]
        history_starts_at_nonce: Option<i64>,
        #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
        history_base_commitment: Option<String>,
    }
    let pending_accounts = || async {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        diesel::sql_query(
            "SELECT account_id, outcome, reason, canonical_nonce, history_starts_at_nonce,
                    history_base_commitment
             FROM account_recovery_pending WHERE account_id LIKE '0xresume%' ORDER BY account_id",
        )
        .load::<Pending>(&mut conn)
        .await
        .unwrap()
    };
    let without_history = |account_id: &str| Pending {
        account_id: account_id.into(),
        outcome: "unresolved".into(),
        reason: "no canonical history supplies a recovery target".into(),
        canonical_nonce: None,
        history_starts_at_nonce: None,
        history_base_commitment: None,
    };
    assert_eq!(
        pending_accounts().await,
        [
            without_history(first),
            without_history(second),
            Pending {
                account_id: joined_later.into(),
                outcome: "unresolved".into(),
                reason: "missing canonical deltas from account creation".into(),
                canonical_nonce: Some(4),
                history_starts_at_nonce: Some(3),
                history_base_commitment: Some("0xheld-base".into()),
            },
        ]
    );
    {
        let mut conn = metadata.pool_handle().get().await.unwrap();
        diesel::sql_query("BEGIN").execute(&mut conn).await.unwrap();
        diesel::sql_query("LOCK TABLE account_recovery_pending, account_recovery_startup IN ACCESS EXCLUSIVE MODE")
            .execute(&mut conn)
            .await
            .unwrap();
        let clear = timeout(Duration::from_secs(5), store.clear_pending(second, &lease)).await;
        let progress = ScanProgress::default();
        let save = timeout(
            Duration::from_secs(5),
            store.save_progress(network, &progress, &lease),
        )
        .await;
        diesel::sql_query("ROLLBACK")
            .execute(&mut conn)
            .await
            .unwrap();
        assert!(clear.unwrap().is_err());
        assert!(save.unwrap().is_err());
    }
    store.clear_pending(second, &lease).await.unwrap();
    assert_eq!(pending_accounts().await.len(), 2);

    let tail = run(
        &checking(&[second, joined_later]),
        &store,
        network,
        ScanWrites::Apply(&lease),
        &CancellationToken::new(),
        first,
        later(),
    )
    .await
    .unwrap();
    assert_eq!((tail.checked, tail.unresolved), (2, 2));
    assert_eq!(tail.last_checked.as_deref(), Some(joined_later));
    assert_eq!(pending_accounts().await.len(), 3);
    assert_eq!(store.remaining_candidates(network, first).await.unwrap(), 2);

    let unreachable = {
        let chain = (0..3).fold(
            crate::testing::mocks::MockNetworkClient::new(),
            |chain, _| chain.with_verify_commitment(Err("simulated RPC outage".into())),
        );
        create_test_app_state_with_mocks(storage.clone(), Arc::new(chain), metadata.clone())
    };
    let outage = run(
        &unreachable,
        &store,
        network,
        ScanWrites::Apply(&lease),
        &CancellationToken::new(),
        "",
        later(),
    )
    .await
    .unwrap();
    assert_eq!((outage.checked, outage.failed), (3, 3));
    assert!(outage.stopped_early);
    assert_eq!(outage.last_checked, None);
    assert_eq!(
        outage.next_step(ScanProgress::default()),
        ScanStep::Resume(ScanProgress::default())
    );

    let flaky = {
        let chain = crate::testing::mocks::MockNetworkClient::new()
            .with_verify_commitment(Err("simulated RPC error".into()))
            .with_verify_commitment(Ok(chain_head(second)))
            .with_verify_commitment(Err("simulated RPC error".into()));
        create_test_app_state_with_mocks(storage.clone(), Arc::new(chain), metadata.clone())
    };
    let interleaved = run(
        &flaky,
        &store,
        network,
        ScanWrites::Apply(&lease),
        &CancellationToken::new(),
        "",
        later(),
    )
    .await
    .unwrap();
    assert_eq!((interleaved.checked, interleaved.failed), (3, 2));
    assert!(!interleaved.stopped_early);
    assert_eq!(interleaved.last_checked.as_deref(), Some(joined_later));

    assert_eq!(
        store.progress(network).await.unwrap(),
        ScanProgress::default()
    );
    let saved = ScanProgress {
        unresolved: 1,
        ..scan_progress(first)
    };
    store.save_progress(network, &saved, &lease).await.unwrap();
    assert_eq!(store.progress(network).await.unwrap(), saved);
    store.leader.release(lease.clone()).await.unwrap();
    assert!(store.save_progress(network, &saved, &lease).await.is_err());

    let stopped = RecoveryStore::new(metadata.pool_handle(), None, "stopped-recovery");
    stopped
        .leader
        .try_acquire(Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        run_within(
            &checking(&[]),
            &store,
            network,
            Duration::ZERO,
            Duration::ZERO
        )
        .await,
        Err(StartupRecoveryError::LeaseBusy)
    ));
    let waited_from = Instant::now();
    assert!(matches!(
        run_within(
            &checking(&[]),
            &store,
            network,
            Duration::ZERO,
            Duration::from_secs(3)
        )
        .await,
        Err(StartupRecoveryError::LeaseBusy)
    ));
    assert!(waited_from.elapsed() >= LEASE_RETRY_INTERVAL);
    let mut conn = metadata.pool_handle().get().await.unwrap();
    diesel::sql_query(
        "UPDATE worker_leases SET expires_at = clock_timestamp() + interval '1 second'
         WHERE lease_name = 'account_recovery'",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    drop(conn);
    run_within(
        &checking(&[]),
        &store,
        network,
        Duration::ZERO,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(store.progress(network).await.unwrap(), saved);

    run_within(
        &checking(&[second, joined_later]),
        &store,
        network,
        Duration::from_secs(60),
        Duration::ZERO,
    )
    .await
    .unwrap();
    assert_eq!(
        store.progress(network).await.unwrap(),
        ScanProgress::default()
    );
    assert!(store.startup_completed(network).await.unwrap());

    #[derive(diesel::QueryableByName)]
    struct Completion {
        #[diesel(sql_type = BigInt)]
        unresolved_accounts: i64,
    }
    let mut conn = metadata.pool_handle().get().await.unwrap();
    let completion = diesel::sql_query(
        "SELECT unresolved_accounts FROM account_recovery_startup
         WHERE network_config = $1 AND completed_at IS NOT NULL",
    )
    .bind::<Jsonb, _>(
        serde_json::to_value(NetworkConfig::Miden {
            network_type: network,
        })
        .unwrap(),
    )
    .get_result::<Completion>(&mut conn)
    .await
    .unwrap();
    assert_eq!(completion.unresolved_accounts, 3);

    diesel::sql_query("UPDATE states SET nonce = 4 WHERE account_id = $1")
        .bind::<Text, _>(joined_later)
        .execute(&mut conn)
        .await
        .unwrap();
    let same_nonce = store
        .candidates(network, second, 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        (same_nonce.stored_nonce, same_nonce.canonical_nonce),
        (Some(4), Some(4))
    );
    let lease = store
        .leader
        .try_acquire(Duration::from_secs(300))
        .await
        .unwrap()
        .unwrap();
    store
        .record_pending(
            &same_nonce,
            PendingOutcome::Deferred,
            "recorded before the restore",
            &lease,
        )
        .await
        .unwrap();
    assert!(
        store
            .apply(
                &same_nonce,
                &serde_json::json!({"restored": true}),
                &Auth::MidenEcdsa {
                    cosigner_commitments: vec![Word::from([1, 2, 3, 4_u32]).into_hex()],
                },
                &ProposalReview::Reviewed(vec![ReviewedProposal {
                    commitment: "0xunreadable-status".into(),
                    status: DeltaStatus::default(),
                    change: ProposalChange::Keep,
                }]),
                chrono::Utc::now(),
                &lease,
            )
            .await
            .unwrap()
    );
    let restored = storage.pull_state_head(joined_later).await.unwrap();
    assert_eq!(
        (restored.commitment.as_str(), restored.nonce),
        ("0xheld-head", Some(4))
    );
    #[derive(diesel::QueryableByName)]
    struct PendingRows {
        #[diesel(sql_type = BigInt)]
        total: i64,
    }
    let mut conn = metadata.pool_handle().get().await.unwrap();
    let pending = diesel::sql_query(
        "SELECT count(*) AS total FROM account_recovery_pending WHERE account_id = $1",
    )
    .bind::<Text, _>(joined_later)
    .get_result::<PendingRows>(&mut conn)
    .await
    .unwrap();
    assert_eq!(pending.total, 0);
    store.leader.release(lease).await.unwrap();
}
