//! Gate 0 spike: drive [`ExecutionDataStore`] under a real chain.
//!
//! Reaching [`TransactionExecutorError::Unauthorized`] is the assertion that matters: it
//! means the transaction kernel executed to completion and stopped only at signature
//! verification, so every query GUARDIAN's own `DataStore` answered — partial account,
//! reference block, partial blockchain, vault and storage-map witnesses, account MAST —
//! was accepted by the VM.

use miden_client::account::AccountInterfaceExt;
use miden_confidential_contracts::masm_builder::get_multisig_library;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Felt;
use miden_protocol::Hasher;
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::account::auth::AuthSecretKey;
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::crypto::dsa::falcon512_poseidon2::SecretKey;
use miden_protocol::crypto::rand::RandomCoin;
use miden_protocol::note::{NoteAttachments, NoteType};
use miden_protocol::testing::account_id::{
    ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET, ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE,
};
use miden_protocol::transaction::{InputNotes, TransactionArgs};
use miden_standards::account::interface::AccountInterface;
use miden_standards::code_builder::CodeBuilder;
use miden_standards::note::P2idNote;
use miden_testing::MockChainBuilder;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};
use miden_tx::{LocalTransactionProver, TransactionExecutor, TransactionExecutorError};

use super::blockchain::authenticated_note_query;
use super::{ExecutionDataStore, verify_against_reference};

/// A multisig+guardian account and a chain containing it.
fn account_and_chain() -> (miden_protocol::account::Account, miden_testing::MockChain) {
    let cosigner = SecretKey::new();
    let config = MultisigGuardianConfig::new(
        1,
        vec![cosigner.public_key().to_commitment()],
        Word::from([9u32, 8, 7, 6]),
    );
    let account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");
    let chain = MockChainBuilder::with_accounts([account.clone()])
        .expect("mock chain accepts account")
        .build()
        .expect("mock chain builds");
    (account, chain)
}

#[tokio::test]
async fn guardian_data_store_serves_a_full_transaction_execution() {
    let (account, chain) = account_and_chain();

    let ref_block = chain.latest_block_header();
    let blockchain = chain.latest_partial_blockchain();

    let store = ExecutionDataStore::new(account.clone(), ref_block.clone(), blockchain, &[])
        .expect("execution data store builds from the account GUARDIAN holds");

    let executor: TransactionExecutor<'_, '_, _, miden_tx::auth::BasicAuthenticator> =
        TransactionExecutor::new(&store);

    let input_notes = InputNotes::default();
    assert!(authenticated_note_query(&input_notes).is_empty());

    let result = executor
        .execute_transaction(
            account.id(),
            ref_block.block_num(),
            input_notes,
            TransactionArgs::default(),
        )
        .await;

    match result {
        Err(TransactionExecutorError::Unauthorized(_)) => {
            // Kernel ran to the auth boundary: every DataStore query was accepted.
        }
        Ok(_) => panic!("expected the unsigned transaction to stop at authorization"),
        Err(other) => panic!("execution failed before the auth boundary: {other:?}"),
    }
}

/// Proves **prepared** authenticated input-note execution: the custom `DataStore` can execute
/// note consumption when the note and its inclusion data are supplied.
///
/// Scope limit, deliberately: `MockChain` supplies both `latest_partial_blockchain()` and the
/// authenticated `InputNote`, so this does **not** show that GUARDIAN can assemble a note
/// block's MMR path from live RPC. That assembly is still pending.
#[tokio::test]
async fn guardian_data_store_serves_note_consumption() {
    let cosigner = SecretKey::new();
    let config = MultisigGuardianConfig::new(
        1,
        vec![cosigner.public_key().to_commitment()],
        Word::from([9u32, 8, 7, 6]),
    );
    let account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");

    let mut builder =
        MockChainBuilder::with_accounts([account.clone()]).expect("mock chain accepts account");

    let faucet_id =
        AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).expect("faucet id is valid");
    let asset: Asset = FungibleAsset::new(faucet_id, 100)
        .expect("asset builds")
        .into();
    let sender_id = AccountId::try_from(ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE)
        .expect("sender id is valid");
    let note = builder
        .add_p2id_note(sender_id, account.id(), &[asset], NoteType::Public)
        .expect("p2id note is added to the chain");

    let chain = builder.build().expect("mock chain builds");

    let ref_block = chain.latest_block_header();
    let blockchain = chain.latest_partial_blockchain();
    let input_note = chain
        .get_public_note(&note.id())
        .expect("note is committed on chain");

    // The notes GUARDIAN holds are handed to the store; nothing is synced.
    let store = ExecutionDataStore::new(
        account.clone(),
        ref_block.clone(),
        blockchain,
        std::slice::from_ref(&note),
    )
    .expect("execution data store builds with input notes");

    let executor: TransactionExecutor<'_, '_, _, miden_tx::auth::BasicAuthenticator> =
        TransactionExecutor::new(&store);

    let note_block = input_note
        .location()
        .expect("authenticated note")
        .block_num();
    let input_notes = InputNotes::new(vec![input_note]).expect("input notes build");
    let note_query = authenticated_note_query(&input_notes);
    assert_eq!(note_query.len(), 1);
    assert_eq!(
        note_query.get(&note.metadata().tag().as_u32()),
        Some(&[note_block].into_iter().collect())
    );

    let result = executor
        .execute_transaction(
            account.id(),
            ref_block.block_num(),
            input_notes,
            TransactionArgs::default(),
        )
        .await;

    match result {
        Err(TransactionExecutorError::Unauthorized(_)) => {
            // The note was authenticated and consumed; execution reached the auth boundary.
        }
        Ok(_) => panic!("expected the unsigned transaction to stop at authorization"),
        Err(other) => panic!("note consumption failed before the auth boundary: {other:?}"),
    }
}

/// The assembly in `blockchain.rs` rests entirely on one invariant: a correctly assembled
/// chain MMR's peaks hash to the reference block's `chain_commitment`. If that holds, the
/// `hash_peaks()` gate is the right correctness check; if it does not, the whole recipe is
/// wrong regardless of transport.
///
/// This asserts it against a real chain rather than a hand-built fixture. It does **not**
/// exercise the RPC path — `SyncChainMmr` delta application still needs a live node.
#[tokio::test]
async fn chain_mmr_peaks_hash_to_the_reference_block_commitment() {
    let (_account, mut chain) = account_and_chain();

    // Check it at more than one height: a single-block chain is a degenerate forest and would
    // hide an off-by-one in how peaks relate to the reference block.
    for _ in 0..3 {
        let reference_block = chain.latest_block_header();
        let blockchain = chain.latest_partial_blockchain();

        verify_against_reference(blockchain.mmr(), &reference_block).unwrap_or_else(|e| {
            panic!(
                "invariant failed at block {}: {e}",
                reference_block.block_num()
            )
        });

        chain.prove_next_block().expect("chain advances");
    }
}

/// The full authorized path through GUARDIAN's own `DataStore`: execute unsigned to learn the
/// summary, sign it as the cosigner *and* as GUARDIAN, re-execute with both signatures in the
/// advice map, then prove.
///
/// This is the first test to get **past** the authorization boundary, so it exercises the
/// signature-advice path and the on-chain guardian gate rather than stopping short of them. It
/// also pins two claims the spec makes but had never exercised:
///
/// - FR-039: `ProvenTransaction::expiration_block_num()` is the authoritative expiration and is
///   available after proving, before submission.
/// - FR-046: that value is finite, so reconciliation has a guaranteed terminal bound.
#[tokio::test]
async fn guardian_executes_signs_and_proves_end_to_end() {
    let cosigner = SecretKey::new();
    let guardian = SecretKey::new();

    let config = MultisigGuardianConfig::new(
        1,
        vec![cosigner.public_key().to_commitment()],
        guardian.public_key().to_commitment(),
    );
    let account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");
    let chain = MockChainBuilder::with_accounts([account.clone()])
        .expect("mock chain accepts account")
        .build()
        .expect("mock chain builds");

    let ref_block = chain.latest_block_header();
    let blockchain = chain.latest_partial_blockchain();
    let store = ExecutionDataStore::new(account.clone(), ref_block.clone(), blockchain, &[])
        .expect("execution data store builds");

    let salt = Word::from([7u32, 7, 7, 7]);

    // Phase 1 — execute unsigned to obtain the summary the cosigners sign.
    let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator> =
        TransactionExecutor::new(&store);
    let mut unsigned = TransactionArgs::default();
    unsigned = unsigned.with_auth_args(salt);

    let summary = match executor
        .execute_transaction(
            account.id(),
            ref_block.block_num(),
            InputNotes::default(),
            unsigned,
        )
        .await
    {
        Err(TransactionExecutorError::Unauthorized(effects)) => effects,
        Ok(_) => panic!("unsigned execution must not authorize"),
        Err(other) => panic!("unsigned execution failed early: {other:?}"),
    };

    let message = summary.as_ref().to_commitment();
    let signing_inputs = SigningInputs::TransactionSummary(summary);

    let cosigner_auth =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(cosigner.clone())]);
    let guardian_auth =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(guardian.clone())]);

    let cosigner_sig = cosigner_auth
        .get_signature(
            cosigner.public_key().to_commitment().into(),
            &signing_inputs,
        )
        .await
        .expect("cosigner signs the summary");
    let guardian_sig = guardian_auth
        .get_signature(
            guardian.public_key().to_commitment().into(),
            &signing_inputs,
        )
        .await
        .expect("guardian signs the summary");

    // Phase 2 — re-execute with the cosigner signature and GUARDIAN's acknowledgment.
    let mut signed = TransactionArgs::default();
    signed = signed.with_auth_args(salt);
    signed.add_signature(
        cosigner.public_key().to_commitment().into(),
        message,
        cosigner_sig,
    );
    signed.add_signature(
        guardian.public_key().to_commitment().into(),
        message,
        guardian_sig,
    );

    let executed = executor
        .execute_transaction(
            account.id(),
            ref_block.block_num(),
            InputNotes::default(),
            signed,
        )
        .await
        .expect("signed execution authorizes and completes");

    // Prove locally. Production delegates this to a remote prover, but the prover receives the
    // same witness either way, so this exercises the identical interface.
    let proven = LocalTransactionProver::default()
        .prove(executed.clone())
        .await
        .expect("proving succeeds");

    // FR-039: the authoritative expiration lives on the proven transaction and is available
    // before submission. Confirmed.
    let expiration = proven.expiration_block_num();

    // FR-046, and this is the finding that matters: a transaction built without an explicit
    // `expiration_delta` is **non-expiring**. The default is the u32::MAX sentinel, not some
    // generous-but-finite horizon.
    //
    // So FR-046's finite-expiration refusal is load-bearing rather than defensive: without it
    // every GUARDIAN execution would sit in the unknown-submission state unbounded, and
    // FR-040's "expired" evidence path would never fire. GUARDIAN cannot fix this itself —
    // setting an expiration would change the transaction and break the summary binding — so
    // the proposal must carry a finite `expiration_delta` from the SDK, and GUARDIAN must
    // refuse proposals that do not.
    //
    // This asserts the sentinel deliberately, so that if upstream ever changes the default the
    // test fails and the requirement gets revisited.
    assert_eq!(
        expiration.as_u32(),
        u32::MAX,
        "a transaction with no expiration_delta is expected to be non-expiring; \
         if this changed upstream, revisit FR-046"
    );

    // The proven transaction must advance the same account the proposal was built against.
    assert_eq!(
        proven.account_id(),
        account.id(),
        "proven tx targets the account"
    );
}

/// Builds a keyed multisig account plus a chain containing it, returning the keys so the
/// caller can sign.
fn signed_account_and_chain() -> (
    miden_protocol::account::Account,
    miden_testing::MockChain,
    SecretKey,
    SecretKey,
) {
    let cosigner = SecretKey::new();
    let guardian = SecretKey::new();
    let config = MultisigGuardianConfig::new(
        1,
        vec![cosigner.public_key().to_commitment()],
        guardian.public_key().to_commitment(),
    );
    let account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");
    let chain = MockChainBuilder::with_accounts([account.clone()])
        .expect("mock chain accepts account")
        .build()
        .expect("mock chain builds");
    (account, chain, cosigner, guardian)
}

/// Runs the two-phase multisig flow against GUARDIAN's `DataStore` and returns the executed
/// transaction. `build_args` is called twice — once unsigned to obtain the summary, once with
/// the signatures folded in — because the transaction must be identical apart from the advice.
async fn execute_two_phase(
    account: &miden_protocol::account::Account,
    chain: &miden_testing::MockChain,
    cosigner: &SecretKey,
    guardian: &SecretKey,
    build_args: impl Fn() -> TransactionArgs,
) -> miden_protocol::transaction::ExecutedTransaction {
    let ref_block = chain.latest_block_header();
    let store = ExecutionDataStore::new(
        account.clone(),
        ref_block.clone(),
        chain.latest_partial_blockchain(),
        &[],
    )
    .expect("execution data store builds");

    let executor: TransactionExecutor<'_, '_, _, BasicAuthenticator> =
        TransactionExecutor::new(&store);

    let summary = match executor
        .execute_transaction(
            account.id(),
            ref_block.block_num(),
            InputNotes::default(),
            build_args(),
        )
        .await
    {
        Err(TransactionExecutorError::Unauthorized(effects)) => effects,
        Ok(_) => panic!("unsigned execution must not authorize"),
        Err(other) => panic!("unsigned execution failed early: {other:?}"),
    };

    let message = summary.as_ref().to_commitment();
    let signing_inputs = SigningInputs::TransactionSummary(summary);

    let cosigner_auth =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(cosigner.clone())]);
    let guardian_auth =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(guardian.clone())]);
    let cosigner_sig = cosigner_auth
        .get_signature(
            cosigner.public_key().to_commitment().into(),
            &signing_inputs,
        )
        .await
        .expect("cosigner signs");
    let guardian_sig = guardian_auth
        .get_signature(
            guardian.public_key().to_commitment().into(),
            &signing_inputs,
        )
        .await
        .expect("guardian signs");

    let mut signed = build_args();
    signed.add_signature(
        cosigner.public_key().to_commitment().into(),
        message,
        cosigner_sig,
    );
    signed.add_signature(
        guardian.public_key().to_commitment().into(),
        message,
        guardian_sig,
    );

    executor
        .execute_transaction(
            account.id(),
            ref_block.block_num(),
            InputNotes::default(),
            signed,
        )
        .await
        .expect("signed execution authorizes and completes")
}

/// **P2ID-send family** driven with the real send script, not `TransactionArgs::default()`.
///
/// The script comes from `AccountInterface::build_send_notes_script`, which is the same
/// upstream primitive the multisig SDK's `build_p2id_transaction_request` uses. The server
/// cannot depend on the SDK, so the script is built from that primitive directly rather than
/// by importing the SDK's builder.
#[tokio::test]
async fn guardian_executes_the_p2id_send_family() {
    let cosigner = SecretKey::new();
    let guardian = SecretKey::new();
    let config = MultisigGuardianConfig::new(
        1,
        vec![cosigner.public_key().to_commitment()],
        guardian.public_key().to_commitment(),
    );
    let mut account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");

    let faucet_id =
        AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).expect("faucet id is valid");
    let asset: Asset = FungibleAsset::new(faucet_id, 100)
        .expect("asset builds")
        .into();

    // Fund the vault before the account enters the chain: sending an asset the account does not
    // hold aborts in the kernel on a vault-balance assertion, which is itself evidence that the
    // vault witnesses this store serves are genuinely being read.
    account
        .vault_mut()
        .add_asset(asset)
        .expect("vault accepts the asset");

    let chain = MockChainBuilder::with_accounts([account.clone()])
        .expect("mock chain accepts funded account")
        .build()
        .expect("mock chain builds");
    let recipient = AccountId::try_from(ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE)
        .expect("recipient id is valid");
    let salt = Word::from([1u32, 2, 3, 4]);

    // The note and script must be identical across both phases, so build them once.
    let mut rng = RandomCoin::new(salt);
    let note = P2idNote::create(
        account.id(),
        recipient,
        vec![asset],
        NoteType::Public,
        NoteAttachments::default(),
        &mut rng,
    )
    .expect("p2id note builds");

    let send_script = AccountInterface::from_account(&account)
        .build_send_notes_script(&[note.clone().into()], None)
        .expect("send script builds");

    let expected_note = note.clone();

    let build_args = || {
        let mut args = TransactionArgs::default()
            .with_tx_script(send_script.clone())
            .with_auth_args(salt);
        args.extend_output_note_recipients(vec![&expected_note]);
        args
    };

    let executed = execute_two_phase(&account, &chain, &cosigner, &guardian, build_args).await;

    assert_eq!(
        executed.output_notes().num_notes(),
        1,
        "the send script must emit exactly the one P2ID note"
    );
    assert_eq!(
        executed.output_notes().get_note(0).id(),
        expected_note.id(),
        "the send script must emit the expected P2ID note"
    );
}

/// **Configuration family** driven with the real `update_signers_and_threshold` script from the
/// multisig MASM library, including its config-hash advice entry.
///
/// The advice payload is reconstructed here rather than imported, since the server cannot
/// depend on the SDK. That reconstruction is self-validating: the MASM procedure recomputes the
/// config hash and aborts on mismatch, so a wrong payload fails this test rather than passing
/// silently.
#[tokio::test]
async fn guardian_executes_the_configuration_family() {
    let (account, chain, cosigner, guardian) = signed_account_and_chain();

    // Rotate to a two-of-two: the existing cosigner plus a new one.
    let new_cosigner = SecretKey::new();
    let signer_commitments = [
        cosigner.public_key().to_commitment(),
        new_cosigner.public_key().to_commitment(),
    ];
    let threshold = 2u64;

    // Mirrors the SDK's `build_multisig_config_advice`: threshold, signer count, two zero
    // felts, then the commitments in reverse order, hashed to give the advice key.
    let mut payload = vec![
        Felt::new_unchecked(threshold),
        Felt::new_unchecked(signer_commitments.len() as u64),
        Felt::new_unchecked(0),
        Felt::new_unchecked(0),
    ];
    for commitment in signer_commitments.iter().rev() {
        payload.extend_from_slice(commitment.as_elements());
    }
    let config_hash: Word = Hasher::hash_elements(&payload);

    let multisig_library = get_multisig_library().expect("multisig library loads");
    let tx_script = CodeBuilder::new()
        .with_dynamically_linked_library(&multisig_library)
        .expect("library links")
        .compile_tx_script(
            r#"
            use oz_multisig::multisig
            begin
                call.multisig::update_signers_and_threshold
            end
            "#,
        )
        .expect("config script compiles");

    let salt = Word::from([5u32, 6, 7, 8]);
    let build_args = || {
        let mut args = TransactionArgs::default()
            .with_tx_script_and_args(tx_script.clone(), config_hash)
            .with_auth_args(salt);
        args.extend_advice_map([(config_hash, payload.clone())]);
        args
    };

    let executed = execute_two_phase(&account, &chain, &cosigner, &guardian, build_args).await;

    assert!(
        !executed.account_delta().storage().is_empty(),
        "a signer/threshold rotation must change account storage"
    );
}

/// FR-051's mechanism, measured: setting a finite `expiration_delta` on the send script yields a
/// finite `expiration_block_num` on the proven transaction, at `reference_block + delta`.
///
/// Together with `guardian_executes_signs_and_proves_end_to_end` — which pins the `u32::MAX`
/// default — this establishes both halves: the default is unbounded, and the client-set delta is
/// what makes reconciliation terminable.
#[tokio::test]
async fn guardian_proves_a_transaction_with_a_finite_expiration() {
    const EXPIRATION_DELTA: u16 = 100;

    let cosigner = SecretKey::new();
    let guardian = SecretKey::new();
    let config = MultisigGuardianConfig::new(
        1,
        vec![cosigner.public_key().to_commitment()],
        guardian.public_key().to_commitment(),
    );
    let mut account = MultisigGuardianBuilder::new(config)
        .build_existing()
        .expect("multisig account builds");

    let faucet_id =
        AccountId::try_from(ACCOUNT_ID_PUBLIC_FUNGIBLE_FAUCET).expect("faucet id is valid");
    let asset: Asset = FungibleAsset::new(faucet_id, 100)
        .expect("asset builds")
        .into();
    account
        .vault_mut()
        .add_asset(asset)
        .expect("vault accepts the asset");

    let chain = MockChainBuilder::with_accounts([account.clone()])
        .expect("mock chain accepts funded account")
        .build()
        .expect("mock chain builds");

    let recipient = AccountId::try_from(ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE)
        .expect("recipient id is valid");
    let salt = Word::from([3u32, 3, 3, 3]);
    let mut rng = RandomCoin::new(salt);
    let note = P2idNote::create(
        account.id(),
        recipient,
        vec![asset],
        NoteType::Public,
        NoteAttachments::default(),
        &mut rng,
    )
    .expect("p2id note builds");

    // The only difference from the send-family test: a finite expiration delta, which the
    // script turns into a `set_tx_expiration` section.
    let send_script = AccountInterface::from_account(&account)
        .build_send_notes_script(&[note.clone().into()], Some(EXPIRATION_DELTA))
        .expect("send script builds with an expiration delta");
    let expected_note = note.clone();

    let build_args = || {
        let mut args = TransactionArgs::default()
            .with_tx_script(send_script.clone())
            .with_auth_args(salt);
        args.extend_output_note_recipients(vec![&expected_note]);
        args
    };

    let ref_block = chain.latest_block_header();
    let executed = execute_two_phase(&account, &chain, &cosigner, &guardian, build_args).await;

    let proven = LocalTransactionProver::default()
        .prove(executed)
        .await
        .expect("proving succeeds");

    let expiration = proven.expiration_block_num();
    assert!(
        expiration.as_u32() < u32::MAX,
        "a client-set expiration_delta must produce a finite expiration, got the \
         non-expiring sentinel"
    );
    assert_eq!(
        expiration.as_u32(),
        ref_block.block_num().as_u32() + u32::from(EXPIRATION_DELTA),
        "expiration must be reference_block + delta"
    );
}
