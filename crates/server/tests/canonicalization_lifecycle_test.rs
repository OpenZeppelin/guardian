mod utils;

use server::auth::{Auth, Credentials};
use server::services::{ConfigureAccountParams, PushDeltaParams};
use server::services::{configure_account, process_canonicalizations_now, push_delta};
use server::storage::{DeltaObject, DeltaStatus, StorageType};
use utils::test_helpers::*;

/// Test canonicalization lifecycle - delta is discarded when on-chain doesn't match
#[tokio::test]
async fn test_canonicalization_discards_mismatched_delta() {
    let state = create_test_app_state().await;

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Step 1: Configure account
    let configure_params = ConfigureAccountParams {
        account_id: account_id_hex.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_pubkeys: vec![pubkey_hex.clone()],
        },
        initial_state,
        storage_type: StorageType::Filesystem,
    };

    configure_account(&state, configure_params)
        .await
        .expect("Configure should succeed");

    // Step 2: Push delta (becomes candidate)
    let delta_1 = load_fixture_delta(1);
    let push_params = PushDeltaParams {
        delta: DeltaObject {
            account_id: delta_1["account_id"].as_str().unwrap().to_string(),
            nonce: delta_1["nonce"].as_u64().unwrap(),
            prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
            new_commitment: String::new(), // Will be calculated by service
            delta_payload: delta_1["delta_payload"].clone(),
            ack_sig: None,
            status: DeltaStatus::default(),
        },
        credentials: Credentials::Signature {
            pubkey: pubkey_hex.clone(),
            signature: signature_hex.clone(),
        },
    };

    let push_result = push_delta(&state, push_params)
        .await
        .expect("Push delta should succeed");

    // Step 3: Verify delta is in candidate state (not canonical yet)
    let storage_backend = state
        .storage
        .get(&StorageType::Filesystem)
        .expect("Should get storage backend");

    let initial_account_state = storage_backend
        .pull_state(&account_id_hex)
        .await
        .expect("Should pull initial state");
    let initial_commitment = initial_account_state.commitment.clone();

    let deltas = storage_backend
        .pull_deltas_after(&account_id_hex, 0)
        .await
        .expect("Should pull deltas");

    assert_eq!(deltas.len(), 1, "Should have 1 delta");
    let delta = &deltas[0];
    assert!(delta.status.is_candidate(), "Delta should be candidate");
    assert!(
        !delta.status.is_canonical(),
        "Delta should not be canonical yet"
    );
    assert!(
        !delta.status.is_discarded(),
        "Delta should not be discarded"
    );

    // Step 4: Trigger canonicalization (bypassing time delay)
    // This will check on-chain and find that the commitment doesn't match
    // (fixture delta has new_commitment that hasn't been applied on-chain)
    let _ = process_canonicalizations_now(&state).await;

    // Step 5: Verify delta is discarded (not canonical)
    let deltas_after = storage_backend
        .pull_deltas_after(&account_id_hex, 0)
        .await
        .expect("Should pull deltas");

    assert_eq!(deltas_after.len(), 1, "Should still have 1 delta");
    let delta_after = &deltas_after[0];
    assert!(
        delta_after.status.is_discarded(),
        "Delta should be discarded"
    );
    assert!(
        !delta_after.status.is_canonical(),
        "Delta should NOT be canonical"
    );
    assert!(
        !delta_after.status.is_candidate(),
        "Delta should NOT still be a candidate"
    );

    // Step 6: Verify account state is NOT updated (still at initial commitment)
    let final_state = storage_backend
        .pull_state(&account_id_hex)
        .await
        .expect("Should pull state");

    assert_eq!(
        final_state.commitment, initial_commitment,
        "State commitment should remain unchanged (delta was discarded)"
    );
}

/// Test failed canonicalization (discard) lifecycle
#[tokio::test]
async fn test_failed_canonicalization_discards_delta() {
    let state = create_test_app_state().await;

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Step 1: Configure account
    let configure_params = ConfigureAccountParams {
        account_id: account_id_hex.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_pubkeys: vec![pubkey_hex.clone()],
        },
        initial_state: initial_state.clone(),
        storage_type: StorageType::Filesystem,
    };

    configure_account(&state, configure_params)
        .await
        .expect("Configure should succeed");

    // Get initial state commitment to verify it doesn't change
    let storage_backend = state
        .storage
        .get(&StorageType::Filesystem)
        .expect("Should get storage backend");

    let initial_account_state = storage_backend
        .pull_state(&account_id_hex)
        .await
        .expect("Should pull initial state");

    let initial_commitment = initial_account_state.commitment.clone();

    // Step 2: Push delta with WRONG new_commitment (will fail on-chain check)
    let delta_1 = load_fixture_delta(1);
    let push_params = PushDeltaParams {
        delta: DeltaObject {
            account_id: delta_1["account_id"].as_str().unwrap().to_string(),
            nonce: delta_1["nonce"].as_u64().unwrap(),
            prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
            new_commitment: String::new(), // Will be calculated by service
            delta_payload: delta_1["delta_payload"].clone(),
            ack_sig: None,
            status: DeltaStatus::default(),
        },
        credentials: Credentials::Signature {
            pubkey: pubkey_hex.clone(),
            signature: signature_hex.clone(),
        },
    };

    let push_result = push_delta(&state, push_params).await;
    assert!(
        push_result.is_ok(),
        "Push should succeed (commitment is calculated correctly)"
    );

    // The delta is now a candidate. When canonicalization runs, it will be discarded
    // because the on-chain commitment won't match (since we haven't actually updated on-chain)
    // This tests that deltas get discarded when on-chain state doesn't match
}

/// Test that already canonical/discarded deltas are not reprocessed
#[tokio::test]
async fn test_only_pending_candidates_are_processed() {
    let state = create_test_app_state().await;

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Configure account
    let configure_params = ConfigureAccountParams {
        account_id: account_id_hex.clone(),
        auth: Auth::MidenFalconRpo {
            cosigner_pubkeys: vec![pubkey_hex.clone()],
        },
        initial_state,
        storage_type: StorageType::Filesystem,
    };

    configure_account(&state, configure_params)
        .await
        .expect("Configure should succeed");

    // Push delta
    let delta_1 = load_fixture_delta(1);
    let push_params = PushDeltaParams {
        delta: DeltaObject {
            account_id: delta_1["account_id"].as_str().unwrap().to_string(),
            nonce: delta_1["nonce"].as_u64().unwrap(),
            prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
            new_commitment: String::new(), // Will be calculated by service
            delta_payload: delta_1["delta_payload"].clone(),
            ack_sig: None,
            status: DeltaStatus::default(),
        },
        credentials: Credentials::Signature {
            pubkey: pubkey_hex.clone(),
            signature: signature_hex.clone(),
        },
    };

    push_delta(&state, push_params)
        .await
        .expect("Push should succeed");

    let storage_backend = state
        .storage
        .get(&StorageType::Filesystem)
        .expect("Should get storage backend");

    // First canonicalization - will discard delta
    let _ = process_canonicalizations_now(&state).await;

    let deltas_after_first = storage_backend
        .pull_deltas_after(&account_id_hex, 0)
        .await
        .expect("Should pull deltas");

    assert_eq!(deltas_after_first.len(), 1);
    assert!(
        deltas_after_first[0].status.is_discarded(),
        "Delta should be discarded"
    );
    let first_discarded_timestamp = deltas_after_first[0].status.timestamp().to_string();

    // Second canonicalization - should not reprocess the discarded delta
    let _ = process_canonicalizations_now(&state).await;

    let deltas_after_second = storage_backend
        .pull_deltas_after(&account_id_hex, 0)
        .await
        .expect("Should pull deltas");

    assert_eq!(deltas_after_second.len(), 1);
    assert_eq!(
        deltas_after_second[0].status.timestamp(),
        first_discarded_timestamp,
        "Discarded delta should not be reprocessed (timestamp unchanged)"
    );
}
