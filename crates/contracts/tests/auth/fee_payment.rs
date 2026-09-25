//! Execution coverage for the guarded-multisig auth args on a fee-charging chain.
//!
//! Since protocol 0.17 the multisig auth components read their auth arg as the commitment to a
//! three-word preimage held in the advice map: the bound block and approval expiration, the salt,
//! and the fee conversion info. `resolve_auth_args` pipes that preimage unconditionally, whatever
//! the chain charges. Every other mock chain in this repository is built with the default
//! `verification_base_fee` of 0, so a non-zero fee reaches the auth procedure nowhere else.
//! These cases are its only coverage outside a manual localnet run.
//!
//! [`reversed_advice_preimage_is_rejected`] pins the advice layout against the MASM implementation.
//! Both SDKs build the preimage from `MultisigAuthArgs` rather than through `miden-client`'s own
//! fee path (the multisig client's `transaction/auth_args.rs` explains why).

use guardian_shared::SignatureScheme;
use miden_confidential_contracts::multisig_guardian::{
    MultisigGuardianBuilder, MultisigGuardianConfig,
};
use miden_protocol::Word;
use miden_protocol::account::{Account, AccountId, AccountType, auth::AuthSecretKey};
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::crypto::SequentialCommit;
use miden_protocol::crypto::dsa::falcon512_poseidon2::{PublicKey, SecretKey};
use miden_protocol::note::NoteType;
use miden_protocol::testing::account_id::{
    ACCOUNT_ID_FEE_FAUCET, ACCOUNT_ID_REGULAR_PUBLIC_ACCOUNT_UPDATABLE_CODE,
};
use miden_protocol::transaction::{ExecutedTransaction, RawOutputNote};
use miden_standards::account::auth::{FeeConversionInfo, MultisigAuthArgs};
use miden_standards::note::TxFeeNote;
use miden_testing::{MockChainBuilder, MockTransactionBuilder};
use miden_tx::TransactionExecutorError;
use miden_tx::auth::{BasicAuthenticator, SigningInputs, TransactionAuthenticator};
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

use super::MultisigAuthArgsExt;

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

/// How the transaction under test carries its multisig auth args.
#[derive(Clone, Copy)]
enum AuthArgsShape {
    /// The three-word preimage a typed create path commits, in the advice map.
    Committed,
    /// The preimage with its words in reverse order, so the commitment does not open to it.
    Reversed,
    /// The commitment alone, with no preimage in the advice map.
    Bare,
}

impl AuthArgsShape {
    fn attach<'a>(
        self,
        transaction: MockTransactionBuilder<'a>,
        auth_args: &MultisigAuthArgs,
    ) -> MockTransactionBuilder<'a> {
        let commitment = auth_args.to_commitment();
        match self {
            Self::Committed => transaction.multisig_auth_args(auth_args),
            Self::Reversed => {
                let preimage = auth_args.to_elements();
                transaction.auth_args(commitment).add_advice_map_entry(
                    commitment,
                    [&preimage[8..], &preimage[4..8], &preimage[..4]].concat(),
                )
            }
            Self::Bare => transaction.auth_args(commitment),
        }
    }
}

/// Runs the whole proposal flow — unsigned execution for the summary, approver and guardian
/// signatures, then signed execution — against a chain that charges a fee.
///
/// The auth args bind the reference block and commit the native 1/1 fee conversion info,
/// which is what a typed create path produces; `shape` decides what the advice map holds.
async fn execute_fee_paying_transaction(
    salt: Word,
    shape: AuthArgsShape,
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

    let auth_args = MultisigAuthArgs::new(chain.latest_block_header().block_num(), salt)
        .with_conversion_info(FeeConversionInfo::one_to_one(fee_faucet_id()?));

    let mut input_notes = vec![funding_note.id()];
    if let Some(spawn_note) = spawn_note.as_ref() {
        input_notes.push(spawn_note.id());
    }

    let build_transaction = || {
        let transaction = shape.attach(
            chain
                .build_transaction(fixture.account.id())
                .authenticated_input_notes(input_notes.clone())
                .authenticator(None),
            &auth_args,
        );
        match user_output_note.clone() {
            Some(note) => transaction.expected_output_note(RawOutputNote::Full(note)),
            None => transaction,
        }
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
        let Some(fee_asset) = asset.as_fungible() else {
            anyhow::bail!("the fee note's asset must be fungible");
        };
        anyhow::ensure!(fee_asset.faucet_id() == fee_faucet_id()?);
        return Ok(fee_asset.amount().as_u64());
    }
    anyhow::bail!("no TX_FEE note was created")
}

/// The auth args a typed create path commits are accepted by the auth procedure at a non-zero
/// verification base fee, and the fee is actually paid: a node rejects a transaction without a
/// canonical TX_FEE note, and the mock chain does not enforce that, so this test does.
#[tokio::test]
async fn committed_conversion_info_pays_the_fee() -> anyhow::Result<()> {
    let salt = Word::from([11u32, 22, 33, 44]);

    let executed = execute_fee_paying_transaction(salt, AuthArgsShape::Committed, false)
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

    let executed = execute_fee_paying_transaction(salt, AuthArgsShape::Committed, true)
        .await?
        .map_err(|error| anyhow::anyhow!("execution must succeed, got: {error}"))?;

    let notes = executed.output_notes();
    assert_eq!(notes.num_notes(), 2, "the user note plus the fee note");
    assert_ne!(notes.get_note(0).metadata().tag(), TxFeeNote::TAG);
    assert_eq!(notes.get_note(1).metadata().tag(), TxFeeNote::TAG);
    assert!(fee_note_amount(&executed)? >= executed.compute_fee().as_u64());

    Ok(())
}

/// The committed auth args pass `resolve_auth_args` and the whole signed flow on a fee-charging
/// chain, so the shape both SDKs build is the one the auth procedure accepts.
#[tokio::test]
async fn committed_auth_args_are_accepted_on_a_fee_charging_chain() -> anyhow::Result<()> {
    let salt = Word::from([11u32, 22, 33, 44]);

    execute_fee_paying_transaction(salt, AuthArgsShape::Committed, false)
        .await?
        .map_err(|error| anyhow::anyhow!("execution must succeed, got: {error}"))?;

    Ok(())
}

/// A preimage whose words are out of order does not open the commitment and must abort.
///
/// The TypeScript SDK builds this preimage by hand and the Rust SDK builds it from
/// `MultisigAuthArgs`. Executing the MASM checks the layout against the kernel instead of relying
/// only on cross-SDK vectors.
#[tokio::test]
async fn reversed_advice_preimage_is_rejected() -> anyhow::Result<()> {
    let salt = Word::from([11u32, 22, 33, 44]);

    let error = execute_fee_paying_transaction(salt, AuthArgsShape::Reversed, false)
        .await?
        .expect_err("a preimage that does not open the commitment must abort");

    assert!(
        !matches!(error, TransactionExecutorError::Unauthorized(_)),
        "the abort must come from resolving the auth args, not from missing signatures: {error}"
    );

    Ok(())
}

/// A bare auth arg with no advice entry aborts before any signature is checked: since 0.17 the
/// multisig pipes the preimage unconditionally, whatever the chain charges.
#[tokio::test]
async fn bare_auth_arg_is_rejected() -> anyhow::Result<()> {
    let salt = Word::from([11u32, 22, 33, 44]);

    let error = execute_fee_paying_transaction(salt, AuthArgsShape::Bare, false)
        .await?
        .expect_err("a bare auth arg must abort");

    assert!(
        error
            .to_string()
            .contains("the advice map holds no preimage for the multisig auth args"),
        "expected ERR_AUTH_ARGS_PREIMAGE_MISSING, got: {error}"
    );

    Ok(())
}
