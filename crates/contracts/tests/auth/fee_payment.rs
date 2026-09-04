//! Execution coverage for the guarded-multisig fee path on a fee-charging chain.
//!
//! Since protocol 0.16 `AuthGuardedMultisig` calls `fee::pay_fee` before the transaction summary
//! is built, so the auth arg must be the commitment `hash(CONVERSION_INFO || SALT)` and the advice
//! map must carry its preimage under that key. Every other mock chain in this repository is built
//! with the default `verification_base_fee` of 0, where the computed fee is zero, no fee note is
//! created and `pay_fee` accepts the empty conversion info — so nothing else reaches this code.
//! These cases are its only coverage outside a manual localnet run.
//!
//! [`reversed_advice_preimage_is_rejected`] pins the advice layout against the MASM implementation.
//! The TypeScript SDK constructs this value itself, while `miden-client` uses
//! `commit_fee_conversion_info` for Rust requests.

use guardian_shared::SignatureScheme;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::account::{Account, AccountId, AccountType, auth::AuthSecretKey};
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::crypto::dsa::falcon512_poseidon2::{PublicKey, SecretKey};
use miden_protocol::note::NoteType;
use miden_protocol::testing::account_id::{
    ACCOUNT_ID_FEE_FAUCET, ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE,
};
use miden_protocol::transaction::{ExecutedTransaction, RawOutputNote};
use miden_protocol::vm::AdviceInputs;
use miden_protocol::{Felt, Word};
use miden_standards::account::auth::{FeeConversionInfo, commit_fee_conversion_info};
use miden_standards::note::TxFeeNote;
use miden_testing::MockChainBuilder;
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

const NUM_APPROVERS: usize = 2;
const VERIFICATION_BASE_FEE: u32 = 500;
const FEE_FUNDING_AMOUNT: u64 = 1_000_000;

/// A guarded-multisig account whose approvers and guardian can all sign.
struct GuardedFixture {
    account: Account,
    approver_keys: Vec<PublicKey>,
    approver_auths: Vec<BasicAuthenticator>,
    guardian_key: PublicKey,
    guardian_auth: BasicAuthenticator,
}

fn guarded_fixture() -> anyhow::Result<GuardedFixture> {
    let mut rng = ChaCha20Rng::from_seed([5u8; 32]);

    let mut approver_keys = Vec::new();
    let mut approver_auths = Vec::new();
    for _ in 0..NUM_APPROVERS {
        let secret_key = SecretKey::with_rng(&mut rng);
        approver_keys.push(secret_key.public_key());
        approver_auths.push(BasicAuthenticator::new(&[
            AuthSecretKey::Falcon512Poseidon2(secret_key),
        ]));
    }

    let guardian_secret_key = SecretKey::with_rng(&mut rng);
    let guardian_key = guardian_secret_key.public_key();
    let guardian_auth =
        BasicAuthenticator::new(&[AuthSecretKey::Falcon512Poseidon2(guardian_secret_key)]);

    let config = MultisigGuardianConfig::new(
        u32::try_from(NUM_APPROVERS)?,
        approver_keys.iter().map(PublicKey::to_commitment).collect(),
        guardian_key.to_commitment(),
    )
    .with_account_type(AccountType::Public)
    .with_signature_scheme(SignatureScheme::Falcon);

    let account = MultisigGuardianBuilder::new(config).build_existing()?;

    Ok(GuardedFixture {
        account,
        approver_keys,
        approver_auths,
        guardian_key,
        guardian_auth,
    })
}

fn fee_faucet_id() -> anyhow::Result<AccountId> {
    Ok(ACCOUNT_ID_FEE_FAUCET.try_into()?)
}

/// The auth arg and advice preimage a typed create path commits for `salt`.
fn native_commitment(salt: Word) -> anyhow::Result<(Word, Vec<Felt>)> {
    Ok(commit_fee_conversion_info(
        FeeConversionInfo::one_to_one(fee_faucet_id()?),
        salt,
    ))
}

/// Runs the whole proposal flow — unsigned execution for the summary, approver and guardian
/// signatures, then signed execution — against a chain that charges a fee.
///
/// `advice_value` is passed through verbatim rather than derived, so a case can supply a preimage
/// the commitment does not open to, or none at all.
async fn execute_fee_paying_transaction(
    auth_args: Word,
    advice_value: Option<Vec<Felt>>,
    with_user_output_note: bool,
) -> anyhow::Result<Result<ExecutedTransaction, TransactionExecutorError>> {
    let fixture = guarded_fixture()?;
    let fee_asset: Asset = FungibleAsset::new(fee_faucet_id()?, FEE_FUNDING_AMOUNT)?.into();
    let counterparty: AccountId = ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE.try_into()?;

    let mut builder = MockChainBuilder::with_accounts([fixture.account.clone()])?
        .verification_base_fee(VERIFICATION_BASE_FEE);

    // The auth procedure runs after note consumption, so a consumed P2ID note is enough to have
    // the fee asset in the vault by the time `pay_fee` withdraws from it.
    let funding_note = builder.add_p2id_note(
        counterparty,
        fixture.account.id(),
        &[fee_asset],
        NoteType::Public,
    )?;

    let user_output_note = if with_user_output_note {
        Some(builder.add_p2id_note(
            fixture.account.id(),
            counterparty,
            &[FungibleAsset::mock(0)],
            NoteType::Public,
        )?)
    } else {
        None
    };
    let spawn_note = match user_output_note.as_ref() {
        Some(note) => Some(builder.add_spawn_note([note])?),
        None => None,
    };

    let chain = builder.build()?;

    let mut input_notes = vec![funding_note.id()];
    if let Some(spawn_note) = spawn_note.as_ref() {
        input_notes.push(spawn_note.id());
    }

    let build_transaction = || {
        let mut transaction = chain
            .build_transaction(fixture.account.id())
            .authenticated_input_notes(input_notes.clone())
            .authenticator(None)
            .auth_args(auth_args);
        if let Some(advice_value) = advice_value.clone() {
            transaction = transaction.extend_advice_inputs(
                AdviceInputs::default().with_map([(auth_args, advice_value)]),
            );
        }
        if let Some(note) = user_output_note.clone() {
            transaction = transaction.expected_output_note(RawOutputNote::Full(note));
        }
        transaction
    };

    let summary = match build_transaction().build()?.execute().await {
        Err(TransactionExecutorError::Unauthorized(summary)) => summary,
        Ok(_) => anyhow::bail!("the unsigned execution must abort as unauthorized"),
        Err(error) => return Ok(Err(error)),
    };

    let msg = summary.as_ref().to_commitment();
    let signing_inputs = SigningInputs::TransactionSummary(summary);

    let mut signed = build_transaction();
    for (key, authenticator) in fixture.approver_keys.iter().zip(&fixture.approver_auths) {
        let signature = authenticator
            .get_signature(key.to_commitment().into(), &signing_inputs)
            .await?;
        signed = signed.add_signature(key.to_commitment().into(), msg, signature);
    }
    let guardian_signature = fixture
        .guardian_auth
        .get_signature(fixture.guardian_key.to_commitment().into(), &signing_inputs)
        .await?;
    signed = signed.add_signature(
        fixture.guardian_key.to_commitment().into(),
        msg,
        guardian_signature,
    );

    Ok(signed.build()?.execute().await)
}

/// Returns the amount of the single native asset in the transaction's TX_FEE note.
fn fee_note_amount(executed: &ExecutedTransaction) -> anyhow::Result<u64> {
    let notes = executed.output_notes();
    for index in 0..notes.num_notes() {
        let note = notes.get_note(index);
        if note.metadata().tag() != TxFeeNote::TAG {
            continue;
        }
        let asset = note
            .assets()
            .iter()
            .next()
            .expect("fee note carries an asset");
        let Asset::Fungible(fee_asset) = asset else {
            anyhow::bail!("the fee note's asset must be fungible");
        };
        anyhow::ensure!(fee_asset.faucet_id() == fee_faucet_id()?);
        return Ok(fee_asset.amount().as_u64());
    }
    anyhow::bail!("no TX_FEE note was created")
}

/// The auth arg and advice preimage a typed create path commits are accepted by the auth
/// procedure at a non-zero verification base fee, and the fee is actually paid.
///
/// Also pins the preimage layout the TypeScript SDK reproduces by hand in
/// `insertFeeConversionInfo`: the advice value is SALT ++ CONVERSION_INFO, the reverse of the
/// commitment's operand order.
#[tokio::test]
async fn committed_conversion_info_pays_the_fee() -> anyhow::Result<()> {
    let salt = Word::from([11u32, 22, 33, 44]);
    let (auth_args, advice_value) = native_commitment(salt)?;

    let conversion_info = FeeConversionInfo::one_to_one(fee_faucet_id()?).to_word();
    assert_eq!(
        advice_value,
        [salt.as_elements(), conversion_info.as_elements()].concat(),
        "load_conversion_info pops the salt first, so the preimage is SALT ++ CONVERSION_INFO"
    );

    let executed = execute_fee_paying_transaction(auth_args, Some(advice_value), false)
        .await?
        .map_err(|error| anyhow::anyhow!("execution must succeed, got: {error}"))?;

    assert_eq!(
        executed.output_notes().num_notes(),
        1,
        "the fee note is the transaction's only output note"
    );
    assert!(fee_note_amount(&executed)? >= executed.compute_fee().as_u64());

    Ok(())
}

/// The fee note is appended after the transaction's own output notes, so `pay_fee` runs with a
/// non-zero output-note count and user note indices are unaffected.
#[tokio::test]
async fn committed_conversion_info_pays_the_fee_alongside_a_user_note() -> anyhow::Result<()> {
    let salt = Word::from([1u32, 2, 3, 4]);
    let (auth_args, advice_value) = native_commitment(salt)?;

    let executed = execute_fee_paying_transaction(auth_args, Some(advice_value), true)
        .await?
        .map_err(|error| anyhow::anyhow!("execution must succeed, got: {error}"))?;

    let notes = executed.output_notes();
    assert_eq!(notes.num_notes(), 2, "the user note plus the fee note");
    assert_ne!(notes.get_note(0).metadata().tag(), TxFeeNote::TAG);
    assert_eq!(notes.get_note(1).metadata().tag(), TxFeeNote::TAG);
    assert!(fee_note_amount(&executed)? >= executed.compute_fee().as_u64());

    Ok(())
}

/// Swapping the halves of the advice preimage must abort.
///
/// The TypeScript SDK builds this preimage by hand, while Rust delegates it to `miden-client`.
/// Executing the MASM checks the layout against the kernel instead of relying only on cross-SDK
/// vectors.
#[tokio::test]
async fn reversed_advice_preimage_is_rejected() -> anyhow::Result<()> {
    let salt = Word::from([11u32, 22, 33, 44]);
    let (auth_args, advice_value) = native_commitment(salt)?;
    let reversed = [&advice_value[4..], &advice_value[..4]].concat();

    let error = execute_fee_paying_transaction(auth_args, Some(reversed), false)
        .await?
        .expect_err("a preimage that does not open the commitment must abort");

    assert!(
        error
            .to_string()
            .contains("does not match the commitment provided via the auth args"),
        "expected the commitment mismatch abort, got: {error}"
    );

    Ok(())
}

/// A bare salt with no advice entry aborts once the computed fee is non-zero, which is the
/// pre-0.16 convention meeting a fee-charging chain.
#[tokio::test]
async fn bare_auth_arg_is_rejected_when_the_fee_is_non_zero() -> anyhow::Result<()> {
    let salt = Word::from([11u32, 22, 33, 44]);

    let error = execute_fee_paying_transaction(salt, None, false)
        .await?
        .expect_err("a bare auth arg must abort on a fee-charging chain");

    assert!(
        error
            .to_string()
            .contains("paying a non-zero fee requires conversion info committed via the auth args"),
        "expected ERR_FEE_CONVERSION_INFO_MISSING, got: {error}"
    );

    Ok(())
}
