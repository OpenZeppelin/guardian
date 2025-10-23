use crate::metadata::auth::{Auth, Credentials};
use crate::services::{ConfigureAccountParams, PushDeltaParams};
use crate::services::{configure_account, push_delta};
use crate::storage::{DeltaObject, DeltaStatus, StorageType};
use crate::testing::helpers::*;
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::Deserializable;
use miden_objects::{Felt, Word};

#[tokio::test]
async fn test_server_signs_commitment_on_push_delta() {
    let state = create_test_app_state().await;

    // Step 1: Get server's public key as hex (automatically generated during state creation)
    let server_pubkey_hex = state.ack.pubkey();

    // Parse the hex string back to PublicKey Word for verification
    let server_pubkey_hex_stripped = server_pubkey_hex.strip_prefix("0x").unwrap_or(&server_pubkey_hex);
    let pubkey_bytes = hex::decode(server_pubkey_hex_stripped).expect("Valid hex");

    let mut pubkey_felts = Vec::new();
    for chunk in pubkey_bytes.chunks(8) {
        let mut arr = [0u8; 8];
        arr[..chunk.len()].copy_from_slice(chunk);
        pubkey_felts.push(Felt::new(u64::from_le_bytes(arr)));
    }
    let pubkey_word: Word = [pubkey_felts[0], pubkey_felts[1], pubkey_felts[2], pubkey_felts[3]].into();
    let server_public_key = miden_objects::crypto::dsa::rpo_falcon512::PublicKey::new(pubkey_word);

    let (_account_id, account_id_hex, initial_state) = load_fixture_account();
    let (_, pubkey_hex, signature_hex) = generate_falcon_signature(&account_id_hex);

    // Step 2: Configure account
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

    // Step 3: Push delta
    let delta_1 = load_fixture_delta(1);
    let push_params = PushDeltaParams {
        delta: DeltaObject {
            account_id: delta_1["account_id"].as_str().unwrap().to_string(),
            nonce: delta_1["nonce"].as_u64().unwrap(),
            prev_commitment: delta_1["prev_commitment"].as_str().unwrap().to_string(),
            new_commitment: String::new(),
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

    // Step 4: Verify the ack signature
    let ack_sig_hex = push_result
        .delta
        .ack_sig
        .as_ref()
        .expect("Should have ack_sig in response");

    assert!(!ack_sig_hex.is_empty(), "ack_sig should not be empty");

    let new_commitment = &push_result.delta.new_commitment;

    let commitment_digest = commitment_to_digest_test(new_commitment);

    let signature_bytes = hex::decode(ack_sig_hex.strip_prefix("0x").unwrap_or(ack_sig_hex))
        .expect("Should decode signature hex");

    let signature = miden_objects::crypto::dsa::rpo_falcon512::Signature::read_from_bytes(
        &mut signature_bytes.as_slice(),
    )
    .expect("Should deserialize signature");

    let is_valid = server_public_key.verify(commitment_digest, &signature);

    assert!(
        is_valid,
        "Server signature should be valid when verified with server public key"
    );

    // Also verify that the signature is stored in the delta
    let storage_backend = state
        .storage
        .get(&StorageType::Filesystem)
        .expect("Should get storage backend");

    let stored_delta = storage_backend
        .pull_delta(&account_id_hex, push_result.delta.nonce)
        .await
        .expect("Should pull stored delta");

    assert_eq!(
        stored_delta.ack_sig,
        Some(ack_sig_hex.clone()),
        "Stored delta should have the same ack_sig"
    );
}

fn commitment_to_digest_test(commitment_hex: &str) -> Word {
    let commitment_hex = commitment_hex.strip_prefix("0x").unwrap_or(commitment_hex);

    let bytes = hex::decode(commitment_hex).expect("Valid hex");

    assert_eq!(bytes.len(), 32, "Commitment must be 32 bytes");

    let mut felts = Vec::new();
    for chunk in bytes.chunks(8) {
        let mut arr = [0u8; 8];
        arr[..chunk.len()].copy_from_slice(chunk);
        let value = u64::from_le_bytes(arr);
        felts.push(Felt::try_from(value).expect("Valid field element"));
    }

    let message_elements = vec![felts[0], felts[1], felts[2], felts[3]];

    let digest = Rpo256::hash_elements(&message_elements);
    digest
}
