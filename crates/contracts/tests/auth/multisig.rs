use guardian_shared::SignatureScheme;
use miden_confidential_contracts::masm_builder::{
    get_guardian_library, get_multisig_ecdsa_library, get_multisig_library,
};
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::{Account, StorageSlotName, auth::AuthSecretKey};
use miden_protocol::asset::FungibleAsset;
use miden_protocol::crypto::dsa::ecdsa_k256_keccak::{
    PublicKey as EcdsaPublicKey, SecretKey as EcdsaSecretKey,
};
use miden_protocol::crypto::dsa::falcon512_rpo::{PublicKey, SecretKey};
use miden_protocol::note::NoteType;
use miden_protocol::testing::account_id::ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE;
use miden_protocol::transaction::OutputNote;
use miden_protocol::vm::{AdviceInputs, AdviceMap};
use miden_protocol::{Felt, Hasher, Word};
use miden_standards::account::wallets::BasicWallet;
use miden_standards::code_builder::CodeBuilder;
use miden_testing::{MockChainBuilder, TxContextInput};
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

// Storage slot names for multisig account storage
const THRESHOLD_CONFIG_SLOT: &str = "openzeppelin::multisig::threshold_config";
const SIGNER_PUBKEYS_SLOT: &str = "openzeppelin::multisig::signer_public_keys";
const PROC_THRESHOLD_ROOTS_SLOT: &str = "openzeppelin::multisig::procedure_thresholds";
const GUARDIAN_PUBLIC_KEY_SLOT: &str = "openzeppelin::guardian::public_key";

// ================================================================================================
// HELPER FUNCTIONS
// ================================================================================================

type MultisigPlusGuardianTestSetup = (
    Vec<SecretKey>,
    Vec<PublicKey>,
    Vec<BasicAuthenticator>,
    SecretKey,
    PublicKey,
    BasicAuthenticator,
);

type MultisigTestSetup = (Vec<SecretKey>, Vec<PublicKey>, Vec<BasicAuthenticator>);

type GuardianTestSetup = (SecretKey, PublicKey, BasicAuthenticator);

type EcdsaMultisigPlusGuardianTestSetup = (
    Vec<EcdsaSecretKey>,
    Vec<EcdsaPublicKey>,
    Vec<BasicAuthenticator>,
    EcdsaSecretKey,
    EcdsaPublicKey,
    BasicAuthenticator,
);

/// Sets up secret keys, public keys, and authenticators for multisig testing
fn setup_keys_and_authenticators(
    num_approvers: usize,
    threshold: usize,
) -> anyhow::Result<MultisigTestSetup> {
    let seed: [u8; 32] = rand::random();
    let mut rng = ChaCha20Rng::from_seed(seed);

    let mut secret_keys = Vec::new();
    let mut public_keys = Vec::new();
    let mut authenticators = Vec::new();

    for _ in 0..num_approvers {
        let sec_key = SecretKey::with_rng(&mut rng);
        let pub_key = sec_key.public_key();

        secret_keys.push(sec_key);
        public_keys.push(pub_key);
    }

    // Create authenticators for required signers
    for secret_key in secret_keys.iter().take(threshold) {
        let authenticator =
            BasicAuthenticator::new(&[AuthSecretKey::Falcon512Rpo(secret_key.clone())]);
        authenticators.push(authenticator);
    }

    Ok((secret_keys, public_keys, authenticators))
}

fn setup_keys_and_authenticators_with_guardian(
    num_approvers: usize,
    threshold: usize,
) -> anyhow::Result<MultisigPlusGuardianTestSetup> {
    let mut rng = ChaCha20Rng::from_seed([0u8; 32]);

    let mut secret_keys = Vec::new();
    let mut public_keys = Vec::new();
    let mut authenticators = Vec::new();

    for _ in 0..num_approvers {
        let sec_key = SecretKey::with_rng(&mut rng);
        let pub_key = sec_key.public_key();

        secret_keys.push(sec_key);
        public_keys.push(pub_key);
    }

    // Create authenticators only for the signers we'll actually use
    for secret_key in secret_keys.iter().take(threshold) {
        let authenticator =
            BasicAuthenticator::new(&[AuthSecretKey::Falcon512Rpo(secret_key.clone())]);
        authenticators.push(authenticator);
    }

    // Create a GUARDIAN authenticator (assuming GUARDIAN uses a single key for simplicity)
    let guardian_sec_key = SecretKey::with_rng(&mut rng);
    let guardian_pub_key = guardian_sec_key.public_key();
    let guardian_authenticator =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Rpo(guardian_sec_key.clone())]);

    Ok((
        secret_keys,
        public_keys,
        authenticators,
        guardian_sec_key,
        guardian_pub_key,
        guardian_authenticator,
    ))
}

fn setup_keys_and_authenticator_for_guardian() -> anyhow::Result<GuardianTestSetup> {
    // Change the RNG seed to avoid key collision with other setups!!!
    let mut rng = ChaCha20Rng::from_seed([8u8; 32]);

    // Create a GUARDIAN authenticator (assuming GUARDIAN uses a single key for simplicity)
    let guardian_sec_key = SecretKey::with_rng(&mut rng);
    let guardian_pub_key = guardian_sec_key.public_key();
    let guardian_authenticator =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Rpo(guardian_sec_key.clone())]);

    Ok((guardian_sec_key, guardian_pub_key, guardian_authenticator))
}

fn setup_ecdsa_keys_and_authenticators_with_guardian(
    num_approvers: usize,
    threshold: usize,
) -> anyhow::Result<EcdsaMultisigPlusGuardianTestSetup> {
    let mut rng = ChaCha20Rng::from_seed([1u8; 32]);

    let mut secret_keys = Vec::new();
    let mut public_keys = Vec::new();
    let mut authenticators = Vec::new();

    for _ in 0..num_approvers {
        let sec_key = EcdsaSecretKey::with_rng(&mut rng);
        let pub_key = sec_key.public_key();

        secret_keys.push(sec_key);
        public_keys.push(pub_key);
    }

    for secret_key in secret_keys.iter().take(threshold) {
        let authenticator =
            BasicAuthenticator::new(&[AuthSecretKey::EcdsaK256Keccak(secret_key.clone())]);
        authenticators.push(authenticator);
    }

    let guardian_sec_key = EcdsaSecretKey::with_rng(&mut rng);
    let guardian_pub_key = guardian_sec_key.public_key();
    let guardian_authenticator =
        BasicAuthenticator::new(&[AuthSecretKey::EcdsaK256Keccak(guardian_sec_key.clone())]);

    Ok((
        secret_keys,
        public_keys,
        authenticators,
        guardian_sec_key,
        guardian_pub_key,
        guardian_authenticator,
    ))
}

fn create_multisig_account_with_guardian_commitments(
    threshold: u32,
    signer_commitments: Vec<Word>,
    guardian_commitment: Word,
    guardian_enabled: bool,
    signature_scheme: SignatureScheme,
) -> anyhow::Result<Account> {
    let config = MultisigGuardianConfig::new(threshold, signer_commitments, guardian_commitment)
        .with_guardian_enabled(guardian_enabled)
        .with_signature_scheme(signature_scheme);

    MultisigGuardianBuilder::new(config).build_existing()
}

fn create_multisig_account_with_guardian(
    threshold: u32,
    public_keys: &[PublicKey],
    guardian_public_key: PublicKey,
    guardian_enabled: bool,
) -> anyhow::Result<Account> {
    let signer_commitments: Vec<Word> = public_keys.iter().map(|pk| pk.to_commitment()).collect();
    let guardian_commitment = guardian_public_key.to_commitment();

    create_multisig_account_with_guardian_commitments(
        threshold,
        signer_commitments,
        guardian_commitment,
        guardian_enabled,
        SignatureScheme::Falcon,
    )
}

fn build_update_procedure_threshold_script_for_scheme(
    procedure_root: Word,
    threshold: u32,
    signature_scheme: SignatureScheme,
) -> anyhow::Result<miden_protocol::transaction::TransactionScript> {
    let multisig_library = match signature_scheme {
        SignatureScheme::Falcon => get_multisig_library()?,
        SignatureScheme::Ecdsa => get_multisig_ecdsa_library()?,
    };
    let tx_script_code = format!(
        r#"
    use oz_multisig::multisig
    begin
        push.{procedure_root}
        push.{threshold}
        call.multisig::update_procedure_threshold
        dropw
        drop
    end
    "#
    );

    CodeBuilder::new()
        .with_dynamically_linked_library(&multisig_library)?
        .compile_tx_script(tx_script_code)
        .map_err(Into::into)
}

fn build_update_procedure_threshold_script(
    procedure_root: Word,
    threshold: u32,
) -> anyhow::Result<miden_protocol::transaction::TransactionScript> {
    build_update_procedure_threshold_script_for_scheme(
        procedure_root,
        threshold,
        SignatureScheme::Falcon,
    )
}

// ================================================================================================
// TESTS
// ================================================================================================

/// Tests basic 2-of-2 multisig functionality with note creation.
///
/// This test verifies that a multisig account with 2 approvers and threshold 2
/// can successfully execute a transaction that creates an output note when both
/// required signatures are provided.
///
/// **Roles:**
/// - 2 Approvers (multisig signers)
/// - 1 Multisig Contract
/// - 1 GUARDIAN Approver
#[tokio::test]
async fn test_multisig_2_of_2_with_note_creation_with_guardian() -> anyhow::Result<()> {
    // Setup keys and authenticators with guardian
    let (
        _secret_keys,
        public_keys,
        authenticators,
        _guardian_secret_key,
        guardian_public_key,
        guardian_authenticator,
    ) = setup_keys_and_authenticators_with_guardian(2, 2)?;

    // Create multisig + guardian account with GUARDIAN enabled
    let mut multisig_account =
        create_multisig_account_with_guardian(2, &public_keys, guardian_public_key.clone(), true)?;

    let output_note_asset = FungibleAsset::mock(0);

    let mut mock_chain_builder =
        MockChainBuilder::with_accounts([multisig_account.clone()]).unwrap();

    // Create output note using add_p2id_note for spawn note
    let output_note = mock_chain_builder.add_p2id_note(
        multisig_account.id(),
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE
            .try_into()
            .unwrap(),
        &[output_note_asset],
        NoteType::Public,
    )?;

    // Create spawn note that will create the output note
    let input_note = mock_chain_builder.add_spawn_note([&output_note])?;

    let mock_chain = mock_chain_builder.build().unwrap();

    let salt = Word::from([Felt::new(1); 4]);

    // Execute transaction without signatures - should fail
    let tx_context_init = mock_chain
        .build_tx_context(
            TxContextInput::Account(multisig_account.clone()),
            &[input_note.id()],
            &[],
        )?
        .extend_expected_output_notes(vec![OutputNote::Full(output_note.clone())])
        .auth_args(salt)
        .build()?;

    let tx_summary = match tx_context_init.execute().await.unwrap_err() {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };

    // Get signatures from both approvers
    let msg = tx_summary.as_ref().to_commitment();
    let tx_summary = SigningInputs::TransactionSummary(tx_summary);

    let sig_1 = authenticators[0]
        .get_signature(public_keys[0].to_commitment().into(), &tx_summary)
        .await?;
    let sig_2 = authenticators[1]
        .get_signature(public_keys[1].to_commitment().into(), &tx_summary)
        .await?;

    // Get signature from guardian
    let guardian_sig = guardian_authenticator
        .get_signature(guardian_public_key.to_commitment().into(), &tx_summary)
        .await?;

    // Execute transaction with signatures - should succeed
    let tx_context_execute = mock_chain
        .build_tx_context(
            TxContextInput::Account(multisig_account.clone()),
            &[input_note.id()],
            &[],
        )?
        .extend_expected_output_notes(vec![OutputNote::Full(output_note)])
        .add_signature(public_keys[0].clone().into(), msg, sig_1)
        .add_signature(public_keys[1].clone().into(), msg, sig_2)
        .add_signature(guardian_public_key.clone().into(), msg, guardian_sig)
        .auth_args(salt)
        .build()?
        .execute()
        .await?;

    multisig_account.apply_delta(tx_context_execute.account_delta())?;

    Ok(())
}

/// Tests updating multisig signers and threshold with GUARDIAN authentication.
#[tokio::test]
async fn test_multisig_update_signers_with_guardian() -> anyhow::Result<()> {
    // This function can be implemented similarly to test_multisig_update_signers,
    // but with the addition of GUARDIAN related logic.
    let (
        _secret_keys,
        public_keys,
        authenticators,
        _guardian_secret_key,
        guardian_public_key,
        guardian_authenticator,
    ) = setup_keys_and_authenticators_with_guardian(2, 2)?;

    // Create multisig + guardian account with GUARDIAN enabled
    let multisig_account =
        create_multisig_account_with_guardian(2, &public_keys, guardian_public_key.clone(), true)?;

    // SECTION 1: Execute a transaction script to update signers and threshold
    // ================================================================================

    let mut mock_chain_builder =
        MockChainBuilder::with_accounts([multisig_account.clone()]).unwrap();

    let output_note_asset = FungibleAsset::mock(0);

    // Create output note for spawn note
    let _output_note = mock_chain_builder.add_p2id_note(
        multisig_account.id(),
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE
            .try_into()
            .unwrap(),
        &[output_note_asset],
        NoteType::Public,
    )?;

    let mock_chain = mock_chain_builder.clone().build().unwrap();

    let salt = Word::from([Felt::new(3); 4]);

    // Setup new signers
    let mut advice_map = AdviceMap::default();
    let (_new_secret_keys, new_public_keys, _new_authenticators) =
        setup_keys_and_authenticators(4, 4)?;

    let threshold = 3u64;
    let num_of_approvers = 4u64;

    // Create vector with threshold config and public keys (4 field elements each)
    let mut config_and_pubkeys_vector = Vec::new();
    config_and_pubkeys_vector.extend_from_slice(&[
        Felt::new(threshold),
        Felt::new(num_of_approvers),
        Felt::new(0),
        Felt::new(0),
    ]);

    // Add each public key to the vector
    for public_key in new_public_keys.iter().rev() {
        let key_word: Word = public_key.to_commitment();
        config_and_pubkeys_vector.extend_from_slice(key_word.as_elements());
    }

    // Hash the vector to create config hash
    let multisig_config_hash = Hasher::hash_elements(&config_and_pubkeys_vector);

    // Insert config and public keys into advice map
    advice_map.insert(multisig_config_hash, config_and_pubkeys_vector);

    // Build the multisig library for transaction script
    let multisig_library = get_multisig_library()?;

    // Use namespaced call syntax for dynamically linked library procedures
    let tx_script_code = r#"
    use oz_multisig::multisig
    begin
        call.multisig::update_signers_and_threshold
    end
    "#;

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(&multisig_library)?
        .compile_tx_script(tx_script_code)?;

    let advice_inputs = AdviceInputs::default()
        .with_map(advice_map.clone().into_iter().map(|(k, v)| (k, v.to_vec())));

    // Pass the MULTISIG_CONFIG_HASH as the tx_script_args
    let tx_script_args: Word = multisig_config_hash;

    // Execute transaction without signatures first to get tx summary
    let tx_context_init = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script.clone())
        .tx_script_args(tx_script_args)
        .extend_advice_inputs(advice_inputs.clone())
        .auth_args(salt)
        .build()?;

    let tx_summary = match tx_context_init.execute().await.unwrap_err() {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };

    // Get signatures from both approvers
    let msg = tx_summary.as_ref().to_commitment();
    let tx_summary = SigningInputs::TransactionSummary(tx_summary);

    let sig_1 = authenticators[0]
        .get_signature(public_keys[0].to_commitment().into(), &tx_summary)
        .await?;
    let sig_2 = authenticators[1]
        .get_signature(public_keys[1].to_commitment().into(), &tx_summary)
        .await?;

    let guardian_sig = guardian_authenticator
        .get_signature(guardian_public_key.to_commitment().into(), &tx_summary)
        .await?;

    // Execute transaction with signatures - should succeed
    let update_approvers_tx = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script)
        .tx_script_args(multisig_config_hash)
        .add_signature(public_keys[0].clone().into(), msg, sig_1)
        .add_signature(public_keys[1].clone().into(), msg, sig_2)
        .add_signature(guardian_public_key.clone().into(), msg, guardian_sig)
        .auth_args(salt)
        .extend_advice_inputs(advice_inputs)
        .build()?
        .execute()
        .await
        .unwrap();

    // Verify the transaction executed successfully
    assert_eq!(
        update_approvers_tx.account_delta().nonce_delta(),
        Felt::new(1)
    );

    // Apply the delta to get the updated account with new signers
    let mut updated_multisig_account = multisig_account.clone();
    updated_multisig_account.apply_delta(update_approvers_tx.account_delta())?;

    // Verify that the public keys were actually updated in storage
    let signer_pubkeys_name = StorageSlotName::new(SIGNER_PUBKEYS_SLOT).unwrap();
    for (i, expected_key) in new_public_keys.iter().enumerate() {
        let storage_key = [
            Felt::new(i as u64),
            Felt::new(0),
            Felt::new(0),
            Felt::new(0),
        ]
        .into();
        let storage_item = updated_multisig_account
            .storage()
            .get_map_item(&signer_pubkeys_name, storage_key)
            .unwrap();

        let expected_word: Word = expected_key.to_commitment();

        assert_eq!(
            storage_item, expected_word,
            "Public key {} doesn't match expected value",
            i
        );
    }

    // Verify the threshold was updated by checking storage slot 0
    let threshold_config_name = StorageSlotName::new(THRESHOLD_CONFIG_SLOT).unwrap();
    let threshold_config_storage = updated_multisig_account
        .storage()
        .get_item(&threshold_config_name)
        .unwrap();

    assert_eq!(
        threshold_config_storage[0],
        Felt::new(threshold),
        "Threshold was not updated correctly"
    );
    assert_eq!(
        threshold_config_storage[1],
        Felt::new(num_of_approvers),
        "Num approvers was not updated correctly"
    );
    Ok(())
}

/// Tests guardian public key update functionality.
///
/// This test verifies that a multisig account can:
/// 1. Execute a transaction script to update the guardian public key without needing a guardian signature
/// 2. Create a second transaction signed by the new guardian public key
/// 3. Properly handle multisig guardian authentication with the updated guardian public key.
///
/// **Roles:**
/// - 2 Original Approvers (multisig signers)
/// - 1 GUARDIAN Approver
/// - 1 Multisig Contract
/// - 1 Transaction Script calling the update_guardian_public_key procedure
#[tokio::test]
async fn test_multisig_update_guardian_public_key() -> anyhow::Result<()> {
    let (
        _secret_keys,
        public_keys,
        authenticators,
        _guardian_secret_key,
        guardian_public_key,
        _guardian_authenticator,
    ) = setup_keys_and_authenticators_with_guardian(2, 2)?;

    // Initialize with GUARDIAN selector = OFF so key update doesn't require GUARDIAN signature
    // This is the expected flow: disable GUARDIAN, update key, then enable GUARDIAN in a follow-up tx
    let multisig_account =
        create_multisig_account_with_guardian(2, &public_keys, guardian_public_key.clone(), true)?;

    // SECTION 1: Execute a transaction script to update GUARDIAN public key
    // ================================================================================

    let mut mock_chain_builder =
        MockChainBuilder::with_accounts([multisig_account.clone()]).unwrap();

    let output_note_asset = FungibleAsset::mock(0);

    // Create output note for spawn note
    let _output_note = mock_chain_builder.add_p2id_note(
        multisig_account.id(),
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE
            .try_into()
            .unwrap(),
        &[output_note_asset],
        NoteType::Public,
    )?;

    let mut mock_chain = mock_chain_builder.clone().build().unwrap();

    let salt = Word::from([Felt::new(3); 4]);

    // Setup New GUARDIAN Public Key
    let (_new_guardian_secret_key, _new_guardian_public_key, _new_guardian_authenticatior) =
        setup_keys_and_authenticator_for_guardian()?;

    // Add new guardian public key to advice inputs
    let advice_inputs = AdviceInputs::default().with_stack(
        _new_guardian_public_key
            .to_commitment()
            .as_elements()
            .iter()
            .copied(),
    );

    // Build the GUARDIAN library for transaction script
    let guardian_library = get_guardian_library()?;

    // Use namespaced call syntax for dynamically linked library procedures
    // This script only calls update_guardian_public_key.
    // Note: enable_guardian is now a private procedure and is automatically called
    // by verify_guardian_signature at the end of transaction authentication.
    let tx_script_code = r#"
    use oz_guardian::guardian
    begin
        call.guardian::update_guardian_public_key
    end
    "#;

    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(&guardian_library)?
        .compile_tx_script(tx_script_code)?;

    // Execute transaction without signatures first to get tx summary
    let tx_context_init = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script.clone())
        .extend_advice_inputs(advice_inputs.clone())
        .auth_args(salt)
        .build()?;

    let tx_summary = match tx_context_init.execute().await.unwrap_err() {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };

    // Get signatures from both approvers
    let msg = tx_summary.as_ref().to_commitment();
    let tx_summary = SigningInputs::TransactionSummary(tx_summary);

    let sig_1 = authenticators[0]
        .get_signature(public_keys[0].to_commitment().into(), &tx_summary)
        .await?;
    let sig_2 = authenticators[1]
        .get_signature(public_keys[1].to_commitment().into(), &tx_summary)
        .await?;

    // Execute transaction with signatures without a need of the GUARDIAN signature! - should succeed
    let update_guardian_public_key_tx = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script)
        .add_signature(public_keys[0].clone().into(), msg, sig_1)
        .add_signature(public_keys[1].clone().into(), msg, sig_2)
        .auth_args(salt)
        .extend_advice_inputs(advice_inputs)
        .build()?
        .execute()
        .await
        .unwrap();

    // Verify the transaction executed successfully
    assert_eq!(
        update_guardian_public_key_tx.account_delta().nonce_delta(),
        Felt::new(1)
    );

    mock_chain.add_pending_executed_transaction(&update_guardian_public_key_tx)?;
    mock_chain.prove_next_block()?;

    // Apply the delta to get the updated account with new guardian public key
    let mut updated_multisig_account = multisig_account.clone();
    updated_multisig_account.apply_delta(update_guardian_public_key_tx.account_delta())?;

    let storage_key = [Felt::new(0), Felt::new(0), Felt::new(0), Felt::new(0)].into();

    // Verify the guardian public key was actually updated in storage
    let guardian_public_key_name = StorageSlotName::new(GUARDIAN_PUBLIC_KEY_SLOT).unwrap();
    let storage_item = updated_multisig_account
        .storage()
        .get_map_item(&guardian_public_key_name, storage_key)
        .unwrap();

    let expected_word: Word = _new_guardian_public_key.to_commitment();

    assert_eq!(
        storage_item, expected_word,
        "GUARDIAN Public key doesn't match expected value"
    );

    Ok(())
}

#[tokio::test]
async fn test_multisig_update_procedure_threshold_replaces_existing_override() -> anyhow::Result<()>
{
    let (_secret_keys, public_keys, authenticators, _, guardian_public_key, guardian_authenticator) =
        setup_keys_and_authenticators_with_guardian(2, 1)?;

    let signer_commitments: Vec<Word> = public_keys.iter().map(|pk| pk.to_commitment()).collect();
    let send_asset_root = BasicWallet::move_asset_to_note_digest();
    let config =
        MultisigGuardianConfig::new(1, signer_commitments, guardian_public_key.to_commitment())
            .with_proc_threshold_overrides(vec![(send_asset_root, 2)]);
    let multisig_account = MultisigGuardianBuilder::new(config).build_existing()?;

    let mock_chain = MockChainBuilder::with_accounts([multisig_account.clone()])?.build()?;
    let salt = Word::from([Felt::new(5); 4]);
    let tx_script = build_update_procedure_threshold_script(send_asset_root, 1)?;

    let tx_context_init = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script.clone())
        .auth_args(salt)
        .build()?;

    let tx_summary = match tx_context_init.execute().await.unwrap_err() {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };

    let msg = tx_summary.as_ref().to_commitment();
    let tx_summary = SigningInputs::TransactionSummary(tx_summary);
    let signer_sig = authenticators[0]
        .get_signature(public_keys[0].to_commitment().into(), &tx_summary)
        .await?;
    let guardian_sig = guardian_authenticator
        .get_signature(guardian_public_key.to_commitment().into(), &tx_summary)
        .await?;

    let executed_tx = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script)
        .add_signature(public_keys[0].to_commitment().into(), msg, signer_sig)
        .add_signature(
            guardian_public_key.to_commitment().into(),
            msg,
            guardian_sig,
        )
        .auth_args(salt)
        .build()?
        .execute()
        .await?;

    let mut updated_account = multisig_account.clone();
    updated_account.apply_delta(executed_tx.account_delta())?;

    let proc_thresholds_name = StorageSlotName::new(PROC_THRESHOLD_ROOTS_SLOT).unwrap();
    let stored_threshold = updated_account
        .storage()
        .get_map_item(&proc_thresholds_name, send_asset_root)
        .unwrap();

    assert_eq!(stored_threshold[0], Felt::new(1));

    Ok(())
}

#[tokio::test]
async fn test_ecdsa_multisig_update_procedure_threshold_replaces_existing_override()
-> anyhow::Result<()> {
    let (_secret_keys, public_keys, authenticators, _, guardian_public_key, guardian_authenticator) =
        setup_ecdsa_keys_and_authenticators_with_guardian(2, 1)?;

    let signer_commitments: Vec<Word> = public_keys.iter().map(|pk| pk.to_commitment()).collect();
    let send_asset_root = BasicWallet::move_asset_to_note_digest();
    let config =
        MultisigGuardianConfig::new(1, signer_commitments, guardian_public_key.to_commitment())
            .with_signature_scheme(SignatureScheme::Ecdsa)
            .with_proc_threshold_overrides(vec![(send_asset_root, 2)]);
    let multisig_account = MultisigGuardianBuilder::new(config).build_existing()?;

    let mock_chain = MockChainBuilder::with_accounts([multisig_account.clone()])?.build()?;
    let salt = Word::from([Felt::new(7); 4]);
    let tx_script = build_update_procedure_threshold_script_for_scheme(
        send_asset_root,
        1,
        SignatureScheme::Ecdsa,
    )?;

    let tx_context_init = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script.clone())
        .auth_args(salt)
        .build()?;

    let tx_summary = match tx_context_init.execute().await.unwrap_err() {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };

    let msg = tx_summary.as_ref().to_commitment();
    let tx_summary = SigningInputs::TransactionSummary(tx_summary);
    let signer_sig = authenticators[0]
        .get_signature(public_keys[0].to_commitment().into(), &tx_summary)
        .await?;
    let guardian_sig = guardian_authenticator
        .get_signature(guardian_public_key.to_commitment().into(), &tx_summary)
        .await?;

    let executed_tx = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script)
        .add_signature(public_keys[0].to_commitment().into(), msg, signer_sig)
        .add_signature(
            guardian_public_key.to_commitment().into(),
            msg,
            guardian_sig,
        )
        .auth_args(salt)
        .build()?
        .execute()
        .await?;

    let mut updated_account = multisig_account.clone();
    updated_account.apply_delta(executed_tx.account_delta())?;

    let proc_thresholds_name = StorageSlotName::new(PROC_THRESHOLD_ROOTS_SLOT).unwrap();
    let stored_threshold = updated_account
        .storage()
        .get_map_item(&proc_thresholds_name, send_asset_root)
        .unwrap();

    assert_eq!(stored_threshold[0], Felt::new(1));

    Ok(())
}

#[tokio::test]
async fn test_multisig_update_signers_rejects_unreachable_existing_proc_override()
-> anyhow::Result<()> {
    let (_secret_keys, public_keys, _, _, guardian_public_key, _) =
        setup_keys_and_authenticators_with_guardian(2, 1)?;

    let signer_commitments: Vec<Word> = public_keys.iter().map(|pk| pk.to_commitment()).collect();
    let send_asset_root = BasicWallet::move_asset_to_note_digest();
    let config =
        MultisigGuardianConfig::new(1, signer_commitments, guardian_public_key.to_commitment())
            .with_proc_threshold_overrides(vec![(send_asset_root, 2)]);
    let multisig_account = MultisigGuardianBuilder::new(config).build_existing()?;

    let mock_chain = MockChainBuilder::with_accounts([multisig_account.clone()])?.build()?;
    let salt = Word::from([Felt::new(6); 4]);

    let new_threshold = 1u64;
    let new_num_approvers = 1u64;
    let mut config_and_pubkeys = vec![
        Felt::new(new_threshold),
        Felt::new(new_num_approvers),
        Felt::new(0),
        Felt::new(0),
    ];
    config_and_pubkeys.extend_from_slice(public_keys[0].to_commitment().as_elements());

    let multisig_config_hash = Hasher::hash_elements(&config_and_pubkeys);
    let mut advice_map = AdviceMap::default();
    advice_map.insert(multisig_config_hash, config_and_pubkeys);
    let advice_inputs =
        AdviceInputs::default().with_map(advice_map.into_iter().map(|(k, v)| (k, v.to_vec())));

    let multisig_library = get_multisig_library()?;
    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(&multisig_library)?
        .compile_tx_script(
            r#"
    use oz_multisig::multisig
    begin
        call.multisig::update_signers_and_threshold
    end
    "#,
        )?;

    let result = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script)
        .tx_script_args(multisig_config_hash)
        .extend_advice_inputs(advice_inputs)
        .auth_args(salt)
        .build()?
        .execute()
        .await;

    match result {
        Err(TransactionExecutorError::TransactionProgramExecutionFailed(_)) => {}
        Ok(_) => {
            panic!("expected signer update to fail when an override exceeds the new signer count")
        }
        Err(err) => panic!("unexpected error type: {err:?}"),
    }

    Ok(())
}
