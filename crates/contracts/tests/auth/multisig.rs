use miden_confidential_contracts::masm_builder::{get_multisig_library, get_psm_library};
use miden_confidential_contracts::multisig_psm::{MultisigPsmBuilder, MultisigPsmConfig};
use miden_lib::note::create_p2id_note;
use miden_lib::utils::ScriptBuilder;
use miden_objects::account::{Account, auth::AuthSecretKey};
use miden_objects::asset::FungibleAsset;
use miden_objects::crypto::dsa::rpo_falcon512::{PublicKey, SecretKey};
use miden_objects::crypto::rand::RpoRandomCoin;
use miden_objects::note::NoteType;
use miden_objects::testing::account_id::ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE;
use miden_objects::transaction::OutputNote;
use miden_objects::vm::{AdviceInputs, AdviceMap};
use miden_objects::{Felt, Hasher, Word};
use miden_testing::utils::create_spawn_note;
use miden_testing::{MockChainBuilder, TxContextInput};
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

// ================================================================================================
// HELPER FUNCTIONS
// ================================================================================================

type MultisigPlusPsmTestSetup = (
    Vec<SecretKey>,
    Vec<PublicKey>,
    Vec<BasicAuthenticator>,
    SecretKey,
    PublicKey,
    BasicAuthenticator,
);

type MultisigTestSetup = (Vec<SecretKey>, Vec<PublicKey>, Vec<BasicAuthenticator>);

type PsmTestSetup = (SecretKey, PublicKey, BasicAuthenticator);

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
            BasicAuthenticator::new(&[AuthSecretKey::RpoFalcon512(secret_key.clone())]);
        authenticators.push(authenticator);
    }

    Ok((secret_keys, public_keys, authenticators))
}

fn setup_keys_and_authenticators_with_psm(
    num_approvers: usize,
    threshold: usize,
) -> anyhow::Result<MultisigPlusPsmTestSetup> {
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
            BasicAuthenticator::new(&[AuthSecretKey::RpoFalcon512(secret_key.clone())]);
        authenticators.push(authenticator);
    }

    // Create a PSM authenticator (assuming PSM uses a single key for simplicity)
    let psm_sec_key = SecretKey::with_rng(&mut rng);
    let psm_pub_key = psm_sec_key.public_key();
    let psm_authenticator =
        BasicAuthenticator::new(&[AuthSecretKey::RpoFalcon512(psm_sec_key.clone())]);

    Ok((
        secret_keys,
        public_keys,
        authenticators,
        psm_sec_key,
        psm_pub_key,
        psm_authenticator,
    ))
}

fn setup_keys_and_authenticator_for_psm() -> anyhow::Result<PsmTestSetup> {
    // Change the RNG seed to avoid key collision with other setups!!!
    let mut rng = ChaCha20Rng::from_seed([8u8; 32]);

    // Create a PSM authenticator (assuming PSM uses a single key for simplicity)
    let psm_sec_key = SecretKey::with_rng(&mut rng);
    let psm_pub_key = psm_sec_key.public_key();
    let psm_authenticator =
        BasicAuthenticator::new(&[AuthSecretKey::RpoFalcon512(psm_sec_key.clone())]);

    Ok((psm_sec_key, psm_pub_key, psm_authenticator))
}

fn create_multisig_account_with_psm(
    threshold: u32,
    public_keys: &[PublicKey],
    psm_public_key: PublicKey,
    psm_enabled: bool,
) -> anyhow::Result<Account> {
    let signer_commitments: Vec<Word> = public_keys.iter().map(|pk| pk.to_commitment()).collect();
    let psm_commitment = psm_public_key.to_commitment();

    let config = MultisigPsmConfig::new(threshold, signer_commitments, psm_commitment)
        .with_psm_enabled(psm_enabled);

    MultisigPsmBuilder::new(config).build_existing()
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
/// - 1 PSM Approver
#[tokio::test]
async fn test_multisig_2_of_2_with_note_creation_with_psm() -> anyhow::Result<()> {
    // Setup keys and authenticators with psm
    let (
        _secret_keys,
        public_keys,
        authenticators,
        _psm_secret_key,
        psm_public_key,
        psm_authenticator,
    ) = setup_keys_and_authenticators_with_psm(2, 2)?;

    // Create multisig + psm account with PSM enabled
    let mut multisig_account =
        create_multisig_account_with_psm(2, &public_keys, psm_public_key.clone(), true)?;

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

    let mut mock_chain = mock_chain_builder.build().unwrap();

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

    // Get signature from psm
    let psm_sig = psm_authenticator
        .get_signature(psm_public_key.to_commitment().into(), &tx_summary)
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
        .add_signature(psm_public_key.clone().into(), msg, psm_sig)
        .auth_args(salt)
        .build()?
        .execute()
        .await?;

    multisig_account.apply_delta(tx_context_execute.account_delta())?;

    mock_chain.add_pending_executed_transaction(&tx_context_execute)?;
    mock_chain.prove_next_block()?;

    // Verify the transaction executed successfully (balance check removed since we don't preload assets)
    Ok(())
}

/// Tests updating multisig signers and threshold with PSM authentication.
#[tokio::test]
async fn test_multisig_update_signers_with_psm() -> anyhow::Result<()> {
    // This function can be implemented similarly to test_multisig_update_signers,
    // but with the addition of PSM related logic.
    let (
        _secret_keys,
        public_keys,
        authenticators,
        _psm_secret_key,
        psm_public_key,
        psm_authenticator,
    ) = setup_keys_and_authenticators_with_psm(2, 2)?;

    // Create multisig + psm account with PSM enabled
    let multisig_account =
        create_multisig_account_with_psm(2, &public_keys, psm_public_key.clone(), true)?;

    // SECTION 1: Execute a transaction script to update signers and threshold
    // ================================================================================

    let mut mock_chain_builder =
        MockChainBuilder::with_accounts([multisig_account.clone()]).unwrap();

    let output_note_asset = FungibleAsset::mock(0);

    // Create output note for spawn note
    let output_note = mock_chain_builder.add_p2id_note(
        multisig_account.id(),
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE
            .try_into()
            .unwrap(),
        &[output_note_asset],
        NoteType::Public,
    )?;

    let mut mock_chain = mock_chain_builder.clone().build().unwrap();

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

    // Use call.:: syntax for dynamically linked library procedure calls (v0.12+)
    let tx_script_code = r#"
    begin
        call.::update_signers_and_threshold
    end
    "#;

    let tx_script = ScriptBuilder::new(true)
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

    let psm_sig = psm_authenticator
        .get_signature(psm_public_key.to_commitment().into(), &tx_summary)
        .await?;

    // Execute transaction with signatures - should succeed
    let update_approvers_tx = mock_chain
        .build_tx_context(TxContextInput::Account(multisig_account.clone()), &[], &[])?
        .tx_script(tx_script)
        .tx_script_args(multisig_config_hash)
        .add_signature(public_keys[0].clone().into(), msg, sig_1)
        .add_signature(public_keys[1].clone().into(), msg, sig_2)
        .add_signature(psm_public_key.clone().into(), msg, psm_sig)
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

    mock_chain.add_pending_executed_transaction(&update_approvers_tx)?;
    mock_chain.prove_next_block()?;

    // Apply the delta to get the updated account with new signers
    let mut updated_multisig_account = multisig_account.clone();
    updated_multisig_account.apply_delta(update_approvers_tx.account_delta())?;

    // Verify that the public keys were actually updated in storage
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
            .get_map_item(1, storage_key)
            .unwrap();

        let expected_word: Word = expected_key.to_commitment();

        assert_eq!(
            storage_item, expected_word,
            "Public key {} doesn't match expected value",
            i
        );
    }

    // Verify the threshold was updated by checking storage slot 0
    let threshold_config_storage = updated_multisig_account.storage().get_item(0).unwrap();

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

    // SECTION 2: Create a second transaction signed by the new owners
    // ================================================================================

    // Now test creating a note with the new signers
    // Setup authenticators for the new signers (we need 3 out of 4 for threshold 3)
    let mut new_authenticators = Vec::new();
    for secret_key in _new_secret_keys.iter().take(3) {
        let authenticator =
            BasicAuthenticator::new(&[AuthSecretKey::RpoFalcon512(secret_key.clone())]);
        new_authenticators.push(authenticator);
    }

    // Create a new output note for the second transaction with new signers
    let output_note_new = create_p2id_note(
        updated_multisig_account.id(),
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE
            .try_into()
            .unwrap(),
        vec![output_note_asset],
        NoteType::Public,
        Default::default(),
        &mut RpoRandomCoin::new(Word::default()),
    )?;

    // Create a new spawn note for the second transaction
    let input_note_new = create_spawn_note([&output_note_new])?;

    let salt_new = Word::from([Felt::new(4); 4]);

    // Build the new mock chain with the updated account and notes
    let mut new_mock_chain_builder =
        MockChainBuilder::with_accounts([updated_multisig_account.clone()]).unwrap();
    new_mock_chain_builder.add_output_note(OutputNote::Full(input_note_new.clone()));
    let new_mock_chain = new_mock_chain_builder.build().unwrap();

    // Execute transaction without signatures first to get tx summary
    let tx_context_init_new = new_mock_chain
        .build_tx_context(
            TxContextInput::Account(updated_multisig_account.clone()),
            &[input_note_new.id()],
            &[],
        )?
        .extend_expected_output_notes(vec![OutputNote::Full(output_note.clone())])
        .auth_args(salt_new)
        .build()?;

    let tx_summary_new = match tx_context_init_new.execute().await.unwrap_err() {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };

    // Get signatures from 3 of the 4 new approvers (threshold is 3)
    let msg_new = tx_summary_new.as_ref().to_commitment();
    let tx_summary_new = SigningInputs::TransactionSummary(tx_summary_new);

    let sig_1_new = new_authenticators[0]
        .get_signature(new_public_keys[0].to_commitment().into(), &tx_summary_new)
        .await?;
    let sig_2_new = new_authenticators[1]
        .get_signature(new_public_keys[1].to_commitment().into(), &tx_summary_new)
        .await?;
    let sig_3_new = new_authenticators[2]
        .get_signature(new_public_keys[2].to_commitment().into(), &tx_summary_new)
        .await?;
    let psm_sig = psm_authenticator
        .get_signature(psm_public_key.to_commitment().into(), &tx_summary_new)
        .await?;

    // SECTION 3: Properly handle multisig authentication with the updated signers
    // ================================================================================

    // Execute transaction with new signatures - should succeed
    let tx_context_execute_new = new_mock_chain
        .build_tx_context(
            TxContextInput::Account(updated_multisig_account.clone()),
            &[input_note_new.id()],
            &[],
        )?
        .extend_expected_output_notes(vec![OutputNote::Full(output_note_new)])
        .add_signature(new_public_keys[0].clone().into(), msg_new, sig_1_new)
        .add_signature(new_public_keys[1].clone().into(), msg_new, sig_2_new)
        .add_signature(new_public_keys[2].clone().into(), msg_new, sig_3_new)
        .add_signature(psm_public_key.clone().into(), msg_new, psm_sig)
        .auth_args(salt_new)
        .build()?
        .execute()
        .await?;

    // Verify the transaction executed successfully with new signers
    assert_eq!(
        tx_context_execute_new.account_delta().nonce_delta(),
        Felt::new(1)
    );

    Ok(())
}

/// Tests psm public key update functionality.
///
/// This test verifies that a multisig account can:
/// 1. Execute a transaction script to update the psm public key without needing a psm signature
/// 2. Create a second transaction signed by the new psm public key
/// 3. Properly handle multisig psm authentication with the updated psm public key.
///
/// **Roles:**
/// - 2 Original Approvers (multisig signers)
/// - 1 PSM Approver
/// - 1 Multisig Contract
/// - 1 Transaction Script calling the update_psm_public_key procedure
#[tokio::test]
async fn test_multisig_update_psm_public_key() -> anyhow::Result<()> {
    let (
        _secret_keys,
        public_keys,
        authenticators,
        _psm_secret_key,
        psm_public_key,
        _psm_authenticator,
    ) = setup_keys_and_authenticators_with_psm(2, 2)?;

    // Initialize with PSM selector = OFF so key update doesn't require PSM signature
    // This is the expected flow: disable PSM, update key, then enable PSM in a follow-up tx
    let multisig_account =
        create_multisig_account_with_psm(2, &public_keys, psm_public_key.clone(), true)?;

    // SECTION 1: Execute a transaction script to update PSM public key
    // ================================================================================

    let mut mock_chain_builder =
        MockChainBuilder::with_accounts([multisig_account.clone()]).unwrap();

    let output_note_asset = FungibleAsset::mock(0);

    // Create output note for spawn note
    let output_note = mock_chain_builder.add_p2id_note(
        multisig_account.id(),
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE
            .try_into()
            .unwrap(),
        &[output_note_asset],
        NoteType::Public,
    )?;

    let mut mock_chain = mock_chain_builder.clone().build().unwrap();

    let salt = Word::from([Felt::new(3); 4]);

    // Setup New PSM Public Key
    let (_new_psm_secret_key, _new_psm_public_key, _new_psm_authenticatior) =
        setup_keys_and_authenticator_for_psm()?;

    // Add new psm public key to advice inputs
    let advice_inputs = AdviceInputs::default().with_stack(
        _new_psm_public_key
            .to_commitment()
            .as_elements()
            .iter()
            .copied(),
    );

    // Build the PSM library for transaction script
    let psm_library = get_psm_library()?;

    // Use call.:: syntax for dynamically linked library procedure calls (v0.12+)
    // This script only calls update_psm_public_key.
    // Note: enable_psm is now a private procedure and is automatically called
    // by verify_psm_signature at the end of transaction authentication.
    let tx_script_code = r#"
    begin
        call.::update_psm_public_key
    end
    "#;

    let tx_script = ScriptBuilder::new(true)
        .with_dynamically_linked_library(&psm_library)?
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

    // Execute transaction with signatures without a need of the PSM signature! - should succeed
    let update_psm_public_key_tx = mock_chain
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
        update_psm_public_key_tx.account_delta().nonce_delta(),
        Felt::new(1)
    );

    mock_chain.add_pending_executed_transaction(&update_psm_public_key_tx)?;
    mock_chain.prove_next_block()?;

    // Apply the delta to get the updated account with new psm public key
    let mut updated_multisig_account = multisig_account.clone();
    updated_multisig_account.apply_delta(update_psm_public_key_tx.account_delta())?;

    let storage_key = [Felt::new(0), Felt::new(0), Felt::new(0), Felt::new(0)].into();

    // Verify the psm public key was actually updated in storage
    let storage_item = updated_multisig_account
        .storage()
        .get_map_item(5, storage_key)
        .unwrap();

    let expected_word: Word = _new_psm_public_key.to_commitment();

    assert_eq!(
        storage_item, expected_word,
        "PSM Public key doesn't match expected value"
    );

    // SECTION 2: Create a second transaction signed by the new PSM public key
    // Now test creating a note with the new psm public key
    // Create a new output note for the second transaction with new psm public key
    let output_note_new = create_p2id_note(
        updated_multisig_account.id(),
        ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE
            .try_into()
            .unwrap(),
        vec![output_note_asset],
        NoteType::Public,
        Default::default(),
        &mut RpoRandomCoin::new(Word::default()),
    )?;

    // Create a new spawn note for the second transaction
    let input_note_new = create_spawn_note([&output_note_new])?;
    let salt_new = Word::from([Felt::new(4); 4]);

    // Build the new mock chain with the updated account and notes
    let mut new_mock_chain_builder =
        MockChainBuilder::with_accounts([updated_multisig_account.clone()]).unwrap();
    new_mock_chain_builder.add_output_note(OutputNote::Full(input_note_new.clone()));
    let new_mock_chain = new_mock_chain_builder.build().unwrap();

    // Execute transaction without signatures first to get tx summary
    let tx_context_init_new = new_mock_chain
        .build_tx_context(
            TxContextInput::Account(updated_multisig_account.clone()),
            &[input_note_new.id()],
            &[],
        )?
        .extend_expected_output_notes(vec![OutputNote::Full(output_note.clone())])
        .auth_args(salt_new)
        .build()?;

    let tx_summary_new = match tx_context_init_new.execute().await.unwrap_err() {
        TransactionExecutorError::Unauthorized(tx_effects) => tx_effects,
        error => panic!("expected abort with tx effects: {error:?}"),
    };

    // Get signatures from approvers
    let msg_new = tx_summary_new.as_ref().to_commitment();
    let tx_summary_new = SigningInputs::TransactionSummary(tx_summary_new);

    let sig_1_new = authenticators[0]
        .get_signature(public_keys[0].to_commitment().into(), &tx_summary_new)
        .await?;
    let sig_2_new = authenticators[1]
        .get_signature(public_keys[1].to_commitment().into(), &tx_summary_new)
        .await?;
    // Get signature from new psm public key
    let psm_sig_new = _new_psm_authenticatior
        .get_signature(_new_psm_public_key.to_commitment().into(), &tx_summary_new)
        .await?;

    assert_ne!(
        _new_psm_public_key.to_commitment(),
        public_keys[0].to_commitment(),
        "PSM public key MUST NOT equal any multisig signer key in this test"
    );

    // SECTION 3: Properly handle multisig PSM authentication with the updated PSM public key
    // Execute transaction with new psm public key - should succeed
    // ================================================================================
    let tx_context_execute_new = new_mock_chain
        .build_tx_context(
            TxContextInput::Account(updated_multisig_account.clone()),
            &[input_note_new.id()],
            &[],
        )?
        .extend_expected_output_notes(vec![OutputNote::Full(output_note_new)])
        .add_signature(public_keys[0].clone().into(), msg_new, sig_1_new)
        .add_signature(public_keys[1].clone().into(), msg_new, sig_2_new)
        .add_signature(_new_psm_public_key.clone().into(), msg_new, psm_sig_new)
        .auth_args(salt_new)
        .build()?
        .execute()
        .await?;

    // Verify the transaction executed successfully with new PSM public key
    assert_eq!(
        tx_context_execute_new.account_delta().nonce_delta(),
        Felt::new(1)
    );

    Ok(())
}
