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
    InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
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
        reconstruct(&network, &account_id, target, deltas.clone()).unwrap(),
        expected
    );
    assert!(
        prove_recovery(&network, &account_id, target, deltas.clone(), Some(1), None)
            .unwrap()
            .is_some()
    );
    assert!(
        prove_recovery(&network, &account_id, target, deltas.clone(), Some(3), None)
            .unwrap()
            .is_none()
    );
    assert!(reconstruct(&network, &account_id, target, vec![deltas[0].clone()]).is_err());
    deltas[1].new_commitment = Some("wrong commitment".into());
    assert!(reconstruct(&network, &account_id, target, deltas).is_err());
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
    assert!(candidate.legacy_network_label);

    let mut absent = state.clone();
    absent.network_client = Arc::new(
        crate::testing::mocks::MockNetworkClient::new()
            .with_verify_commitment(Ok(StateVerification::Absent))
            .with_verify_commitment(Ok(StateVerification::Absent)),
    );
    assert!(matches!(
        check_candidate(&absent, &candidate).await.unwrap(),
        RecoveryOutcome::Retryable(reason) if reason.contains("no record of the account")
    ));
    let mut undeployed = store
        .candidates(MidenNetworkType::Local, "", 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(!undeployed.legacy_network_label);
    undeployed.stored_nonce = Some(0);
    undeployed.canonical_nonce = None;
    undeployed.canonical_commitment = None;
    assert!(matches!(
        check_candidate(&absent, &undeployed).await.unwrap(),
        RecoveryOutcome::Unchanged
    ));
    let mut labelled_without_history = store
        .candidates(MidenNetworkType::Testnet, "", 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert!(labelled_without_history.legacy_network_label);
    labelled_without_history.stored_nonce = Some(5);
    labelled_without_history.canonical_nonce = None;
    labelled_without_history.canonical_commitment = None;
    let mut still_absent = state.clone();
    still_absent.network_client = Arc::new(
        crate::testing::mocks::MockNetworkClient::new()
            .with_verify_commitment(Ok(StateVerification::Absent)),
    );
    assert!(matches!(
        check_candidate(&still_absent, &labelled_without_history)
            .await
            .unwrap(),
        RecoveryOutcome::Unchanged
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
            &lease,
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

    chain.set_on_chain_commitment(account_id, "different head");
    assert!(matches!(
        check_candidate(&state, &candidate).await.unwrap(),
        RecoveryOutcome::Retryable(reason) if reason.contains("current chain head")
    ));
    chain.set_on_chain_commitment(account_id, target);
    let mut ahead = store
        .candidates(MidenNetworkType::Testnet, "", 10)
        .await
        .unwrap()
        .pop()
        .unwrap();
    ahead.stored_nonce = Some(9);
    assert!(matches!(
        check_candidate(&state, &ahead).await.unwrap(),
        RecoveryOutcome::Unresolved(reason) if reason.contains("newer than the verified history")
    ));
    let pending = DeltaObject {
        nonce: 3,
        prev_commitment: target.into(),
        new_commitment: None,
        status: DeltaStatus::candidate("2026-11-14T12:30:00Z".into()),
        ..deltas[0].clone()
    };
    storage.submit_delta(&pending).await.unwrap();
    assert!(matches!(
        check_candidate(&state, &candidate).await.unwrap(),
        RecoveryOutcome::Verified(_, _)
    ));
    assert!(
        !store
            .apply(
                &candidate,
                &canonical.state_json,
                &expected_auth,
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
            DeltaStatus::Discarded {
                timestamp: "2026-11-14T12:40:00Z".into(),
                reason: None,
            },
        )
        .await
        .unwrap();

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
    assert_eq!(
        metadata
            .get_for_read(account_id)
            .await
            .unwrap()
            .unwrap()
            .auth,
        expected_auth
    );

    #[derive(diesel::QueryableByName)]
    struct Backup {
        #[diesel(sql_type = BigInt)]
        total: i64,
        #[diesel(sql_type = Jsonb)]
        prior_state: serde_json::Value,
        #[diesel(sql_type = Jsonb)]
        prior_auth: serde_json::Value,
    }
    let mut conn = metadata.pool_handle().get().await.unwrap();
    let backup = diesel::sql_query("SELECT count(*) OVER () AS total, prior_state, prior_auth FROM account_recovery_backups WHERE account_id = $1")
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
    }
}

#[test]
fn a_scan_completes_only_from_a_whole_pass_that_deferred_no_account() {
    let interrupted = ScanSummary {
        unresolved: 2,
        last_checked: Some("0xb".into()),
        stopped_early: true,
        ..Default::default()
    };
    assert_eq!(
        interrupted.next_step(scan_progress("0xa")),
        ScanStep::Resume(scan_progress("0xb"))
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
        finished.next_step(scan_progress("0xa")),
        ScanStep::Resume(ScanProgress::default())
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
        &lease,
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
        &lease,
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
        &lease,
        &CancellationToken::new(),
        "",
        later(),
    )
    .await
    .unwrap();
    assert_eq!((outage.checked, outage.failed), (3, 3));
    assert!(outage.stopped_early);
    assert_eq!(outage.last_checked, None);

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
        &lease,
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
    let saved = scan_progress(first);
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
    assert!(!store.startup_completed(network).await.unwrap());

    run_within(
        &checking(&[first, second, joined_later]),
        &store,
        network,
        Duration::from_secs(60),
        Duration::ZERO,
    )
    .await
    .unwrap();
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
    assert!(
        store
            .apply(
                &same_nonce,
                &serde_json::json!({"restored": true}),
                &Auth::MidenEcdsa {
                    cosigner_commitments: vec![Word::from([1, 2, 3, 4_u32]).into_hex()],
                },
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
    store.leader.release(lease).await.unwrap();
}
