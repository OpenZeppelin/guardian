//! Multisig auth args: what the 0.17 guarded-multisig auth procedure reads out
//! of a transaction's auth argument.
//!
//! The auth arg is the commitment to a three-word preimage the request carries
//! in its advice map: the block the summary binds together with the approval
//! expiration, the salt, and the fee conversion info. miden-client's own fee
//! path still commits the two-word `CONVERSION_INFO || SALT` pair a fixed-salt
//! component reads, which the multisig aborts on while piping the preimage
//! (`advice stack read failed`), so the request has to carry the three-word
//! shape before the client sees it. The client leaves a request that already
//! has an auth arg alone.

use std::num::NonZeroU32;

use miden_client::transaction::{ChainAnchor, TransactionRequestBuilder, TransactionSummary};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::BlockNumber;
use miden_protocol::crypto::SequentialCommit;
use miden_standards::account::auth::{FeeConversionInfo, MultisigAuthArgs};

use crate::MidenSdkClient;
use crate::error::{MultisigError, Result};

/// Where the six user params a multisig binds into the summary hold the
/// approval expiration block (zero for an approval that never expires) and the
/// salt.
const APPROVAL_EXPIRATION_USER_PARAM_INDEX: usize = 0;
const SALT_USER_PARAM_OFFSET: usize = 2;

/// Builds the auth args for a request executed at `bound_block_num` on the
/// chain whose fee faucet is `fee_faucet_id`.
///
/// The fee conversion info names that faucet at rate 1/1, the only conversion
/// `pay_fee` accepts. The faucet is the one the client's protocol configuration
/// was registered from, so this needs no store read.
pub fn multisig_auth_args(
    fee_faucet_id: AccountId,
    bound_block_num: BlockNumber,
    salt: Word,
    approval_expiration_delta: Option<NonZeroU32>,
) -> Result<MultisigAuthArgs> {
    let auth_args = MultisigAuthArgs::new(bound_block_num, salt)
        .with_conversion_info(FeeConversionInfo::one_to_one(fee_faucet_id));
    match approval_expiration_delta {
        Some(delta) => auth_args
            .with_approval_expiration_delta(delta)
            .map_err(|e| {
                MultisigError::InvalidConfig(format!("invalid approval expiration delta: {e}"))
            }),
        None => Ok(auth_args),
    }
}

/// Auth args for a request a proposer builds now, bound to `client`'s sync
/// height: the anchor captured right after names that block, and
/// [`execute_for_summary`](crate::transaction::execute_for_summary) refuses the
/// pair otherwise.
pub async fn proposer_auth_args(
    client: &MidenSdkClient,
    fee_faucet_id: AccountId,
    salt: Word,
    approval_expiration_delta: Option<NonZeroU32>,
) -> Result<MultisigAuthArgs> {
    let sync_height = client.get_sync_height().await.map_err(|e| {
        MultisigError::miden_client_with_context(
            "failed to read the sync height for the auth args",
            e,
        )
    })?;
    multisig_auth_args(fee_faucet_id, sync_height, salt, approval_expiration_delta)
}

/// Auth args that rebuild an existing proposal's request: the salt and approval
/// expiration its signed summary binds, at the anchor's block. Anything else
/// reproduces a summary the cosigners never signed.
pub fn proposal_auth_args(
    fee_faucet_id: AccountId,
    summary: &TransactionSummary,
    chain_anchor: &ChainAnchor,
) -> Result<MultisigAuthArgs> {
    let bound_block_num = chain_anchor.block_num();
    multisig_auth_args(
        fee_faucet_id,
        bound_block_num,
        summary_salt(summary),
        approval_expiration_delta_of(summary, bound_block_num)?,
    )
}

/// Attaches multisig auth args to a request under construction.
pub trait TransactionRequestBuilderExt {
    /// Sets `auth_args` as the request's auth arg and puts its preimage in the
    /// advice map. A request declaring `fee_conversion_salt` instead would have
    /// miden-client commit the two-word pair over this.
    fn multisig_auth_args(self, auth_args: &MultisigAuthArgs) -> Self;
}

impl TransactionRequestBuilderExt for TransactionRequestBuilder {
    fn multisig_auth_args(self, auth_args: &MultisigAuthArgs) -> Self {
        let commitment = auth_args.to_commitment();
        self.auth_arg(commitment)
            .extend_advice_map([(commitment, auth_args.to_elements())])
    }
}

/// The salt a multisig transaction summary binds.
///
/// Since protocol 0.17 the multisig auth components bind the salt itself into
/// the summary's user params rather than a commitment derived from it, so the
/// value the cosigners signed over is readable again. A proposal still carries
/// the salt in its metadata, because a request has to be rebuilt before any
/// summary exists; this is the cross-check that the two agree.
pub fn summary_salt(summary: &TransactionSummary) -> Word {
    let user_params = summary.user_params();
    let elements = user_params.as_elements();
    Word::new([
        elements[SALT_USER_PARAM_OFFSET],
        elements[SALT_USER_PARAM_OFFSET + 1],
        elements[SALT_USER_PARAM_OFFSET + 2],
        elements[SALT_USER_PARAM_OFFSET + 3],
    ])
}

/// The block at which the approvers' signatures stop authorizing the
/// transaction, or `None` for an approval that never expires.
pub fn summary_approval_expiration_block_num(summary: &TransactionSummary) -> Option<BlockNumber> {
    let user_params = summary.user_params();
    let value = user_params.as_elements()[APPROVAL_EXPIRATION_USER_PARAM_INDEX].as_canonical_u64();
    (value != 0).then(|| BlockNumber::from(value as u32))
}

fn approval_expiration_delta_of(
    summary: &TransactionSummary,
    bound_block_num: BlockNumber,
) -> Result<Option<NonZeroU32>> {
    let Some(expiration) = summary_approval_expiration_block_num(summary) else {
        return Ok(None);
    };
    expiration
        .as_u32()
        .checked_sub(bound_block_num.as_u32())
        .and_then(NonZeroU32::new)
        .map(Some)
        .ok_or_else(|| {
            MultisigError::InvalidConfig(format!(
                "approval expires at block {expiration}, at or before the block {bound_block_num} \
                 its summary binds"
            ))
        })
}
