use crate::testing::helpers::create_test_app_state;
use miden_objects::crypto::dsa::rpo_falcon512::SecretKey;
use miden_objects::crypto::hash::rpo::Rpo256;
use miden_objects::utils::Serializable;
use miden_objects::{Felt, FieldElement, Word};

#[tokio::test]
async fn test_keystore_add_and_retrieve_key() {
    let state = create_test_app_state().await;

    let secret_key = SecretKey::new();
    let public_key = secret_key.public_key();
    let pub_key_word: Word = public_key.into();

    state
        .keystore
        .add_key(&secret_key)
        .expect("Failed to add key");

    let retrieved_key = state
        .keystore
        .get_key(pub_key_word)
        .expect("Failed to retrieve key");

    assert_eq!(
        secret_key.to_bytes(),
        retrieved_key.to_bytes(),
        "Retrieved key should match original"
    );
}

#[tokio::test]
async fn test_keystore_sign_message() {
    let state = create_test_app_state().await;

    let secret_key = SecretKey::new();
    let public_key = secret_key.public_key();
    let pub_key_word: Word = public_key.into();

    state
        .keystore
        .add_key(&secret_key)
        .expect("Failed to add key");

    let message: Word = [Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)].into();

    let signature = state
        .keystore
        .sign(pub_key_word, message)
        .expect("Failed to sign message");

    assert!(
        public_key.verify(message, &signature),
        "Signature should be valid"
    );
}

#[tokio::test]
async fn test_keystore_sign_account_id() {
    let state = create_test_app_state().await;

    let secret_key = SecretKey::new();
    let public_key = secret_key.public_key();
    let pub_key_word: Word = public_key.into();

    state
        .keystore
        .add_key(&secret_key)
        .expect("Failed to add key");

    use miden_objects::account::{AccountId, AccountIdVersion, AccountStorageMode, AccountType};

    let account_id = AccountId::dummy(
        [0u8; 15],
        AccountIdVersion::Version0,
        AccountType::RegularAccountImmutableCode,
        AccountStorageMode::Private,
    );

    let account_id_felts: [Felt; 2] = account_id.into();
    let message_elements = vec![
        account_id_felts[0],
        account_id_felts[1],
        Felt::ZERO,
        Felt::ZERO,
    ];
    let message = Rpo256::hash_elements(&message_elements);

    let signature = state
        .keystore
        .sign(pub_key_word, message)
        .expect("Failed to sign account ID");

    assert!(
        public_key.verify(message, &signature),
        "Account ID signature should be valid"
    );
}

#[tokio::test]
async fn test_keystore_get_nonexistent_key() {
    let state = create_test_app_state().await;

    let fake_pub_key: Word = [Felt::new(99), Felt::new(88), Felt::new(77), Felt::new(66)].into();

    let result = state.keystore.get_key(fake_pub_key);

    assert!(
        result.is_err(),
        "Getting nonexistent key should return error"
    );
}

#[tokio::test]
async fn test_keystore_multiple_keys() {
    let state = create_test_app_state().await;

    let secret_key1 = SecretKey::new();
    let public_key1 = secret_key1.public_key();
    let pub_key_word1: Word = public_key1.into();

    let secret_key2 = SecretKey::new();
    let public_key2 = secret_key2.public_key();
    let pub_key_word2: Word = public_key2.into();

    state
        .keystore
        .add_key(&secret_key1)
        .expect("Failed to add first key");

    state
        .keystore
        .add_key(&secret_key2)
        .expect("Failed to add second key");

    let retrieved_key1 = state
        .keystore
        .get_key(pub_key_word1)
        .expect("Failed to retrieve first key");

    let retrieved_key2 = state
        .keystore
        .get_key(pub_key_word2)
        .expect("Failed to retrieve second key");

    assert_eq!(
        secret_key1.to_bytes(),
        retrieved_key1.to_bytes(),
        "First key should match"
    );

    assert_eq!(
        secret_key2.to_bytes(),
        retrieved_key2.to_bytes(),
        "Second key should match"
    );

    assert_ne!(
        retrieved_key1.to_bytes(),
        retrieved_key2.to_bytes(),
        "Keys should be different"
    );
}

#[tokio::test]
async fn test_keystore_signature_verification_with_wrong_key() {
    let state = create_test_app_state().await;

    let secret_key1 = SecretKey::new();
    let public_key1 = secret_key1.public_key();
    let pub_key_word1: Word = public_key1.into();

    let secret_key2 = SecretKey::new();
    let public_key2 = secret_key2.public_key();

    state
        .keystore
        .add_key(&secret_key1)
        .expect("Failed to add key");

    let message: Word = [Felt::new(1), Felt::new(2), Felt::new(3), Felt::new(4)].into();

    let signature = state
        .keystore
        .sign(pub_key_word1, message)
        .expect("Failed to sign message");

    assert!(
        !public_key2.verify(message, &signature),
        "Signature should be invalid with wrong public key"
    );
}
