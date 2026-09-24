//! Multisig auth args: what the 0.17 guarded-multisig auth procedure reads out
//! of a transaction's auth argument.
//!
//! The auth arg is the commitment to a three-word preimage the request carries
//! in its advice map: the block the summary binds together with the approval
//! expiration, the salt, and the fee conversion info. This crate sets it on the
//! request itself and never declares `fee_conversion_salt`: the client leaves a
//! request that already has an auth arg alone, whereas its own fee path (as of
//! miden-client 0.17.0-rc.1) commits the two-word `CONVERSION_INFO || SALT`
//! pair a fixed-salt component reads, which the multisig aborts on while piping
//! the preimage (`advice stack read failed`). This is the one place that
//! rationale lives; other comments refer here.

use std::num::NonZeroU32;

use miden_client::transaction::{ChainAnchor, TransactionRequestBuilder, TransactionSummary};
use miden_protocol::Word;
use miden_protocol::account::AccountId;
use miden_protocol::block::{BlockHeader, BlockNumber};
use miden_protocol::crypto::SequentialCommit;
use miden_standards::account::auth::{FeeConversionInfo, MultisigAuthArgs};

use crate::MidenSdkClient;
use crate::error::{MultisigError, Result};

/// Where the six user params a multisig binds into the summary hold the
/// approval expiration block (zero for an approval that never expires) and the
/// salt.
const APPROVAL_EXPIRATION_USER_PARAM_INDEX: usize = 0;
const SALT_USER_PARAM_OFFSET: usize = 2;

/// The furthest a transaction may expire after its reference block
/// (`MAX_EXPIRATION_BLOCK_DELTA` in the transaction kernel). The auth procedure
/// clamps the approval expiration it applies to this, so a longer approval
/// would outlive the transaction it authorizes: the summary would still say
/// "valid" while the node already refuses the submission as expired.
pub const MAX_APPROVAL_EXPIRATION_DELTA: u32 = 65_535;

/// Builds the auth args for a request executed at `bound_block_num` on the
/// chain whose fee faucet is `fee_faucet_id`.
///
/// The fee conversion info names that faucet at rate 1/1, the only conversion
/// `pay_fee` accepts.
pub fn multisig_auth_args(
    fee_faucet_id: AccountId,
    bound_block_num: BlockNumber,
    salt: Word,
    approval_expiration_delta: Option<NonZeroU32>,
) -> Result<MultisigAuthArgs> {
    let auth_args = MultisigAuthArgs::new(bound_block_num, salt)
        .with_conversion_info(FeeConversionInfo::one_to_one(fee_faucet_id));
    let Some(delta) = approval_expiration_delta else {
        return Ok(auth_args);
    };
    if delta.get() > MAX_APPROVAL_EXPIRATION_DELTA {
        return Err(MultisigError::InvalidConfig(format!(
            "approval expiration delta {delta} exceeds the {MAX_APPROVAL_EXPIRATION_DELTA} blocks a \
             transaction can stay valid for; the auth procedure would clamp it and the approval \
             would outlive the transaction"
        )));
    }
    auth_args
        .with_approval_expiration_delta(delta)
        .map_err(|e| {
            MultisigError::InvalidConfig(format!("invalid approval expiration delta: {e}"))
        })
}

/// The chain's fee faucet, read from the protocol configuration that `header`
/// commits to. The client stores every configuration a sync delivers, so this
/// is a local read; it fails only for a header the store has no configuration
/// for, such as one from before the client ever synced.
async fn fee_faucet_id_at(client: &MidenSdkClient, header: &BlockHeader) -> Result<AccountId> {
    let protocol_config = client
        .get_protocol_config(header.protocol_config_commitment())
        .await
        .map_err(|e| {
            MultisigError::miden_client_with_context(
                format!(
                    "no protocol configuration is stored for block {}; sync the client first",
                    header.block_num()
                ),
                e,
            )
        })?;
    Ok(protocol_config.fee_asset_id().faucet_id())
}

/// The fee faucet of the protocol configuration at the client's sync height.
pub async fn synced_fee_faucet_id(client: &MidenSdkClient) -> Result<AccountId> {
    let header = client.get_latest_block_header().await.map_err(|e| {
        MultisigError::miden_client_with_context("failed to read the latest block header", e)
    })?;
    fee_faucet_id_at(client, &header).await
}

/// Auth args for a request a proposer builds now, bound to `client`'s sync
/// height: the anchor captured right after names that block, and
/// `execute_for_summary` refuses the pair otherwise.
pub async fn proposer_auth_args(
    client: &MidenSdkClient,
    salt: Word,
    approval_expiration_delta: Option<NonZeroU32>,
) -> Result<MultisigAuthArgs> {
    let header = client.get_latest_block_header().await.map_err(|e| {
        MultisigError::miden_client_with_context(
            "failed to read the latest block header for the auth args",
            e,
        )
    })?;
    multisig_auth_args(
        fee_faucet_id_at(client, &header).await?,
        header.block_num(),
        salt,
        approval_expiration_delta,
    )
}

/// Auth args that rebuild an existing proposal's request: the salt and approval
/// expiration its signed summary binds, at the anchor's block, with the fee
/// faucet of the configuration that block commits to (the one the anchored
/// execution loads). Anything else reproduces a summary the cosigners never
/// signed.
pub async fn proposal_auth_args(
    client: &MidenSdkClient,
    summary: &TransactionSummary,
    chain_anchor: &ChainAnchor,
) -> Result<MultisigAuthArgs> {
    let bound_block_num = chain_anchor.block_num();
    multisig_auth_args(
        fee_faucet_id_at(client, chain_anchor.header()).await?,
        bound_block_num,
        summary_salt(summary),
        approval_expiration_delta_of(summary, bound_block_num)?,
    )
}

/// Attaches multisig auth args to a request under construction.
pub trait TransactionRequestBuilderExt {
    /// Sets `auth_args` as the request's auth arg and puts its preimage in the
    /// advice map. A request declaring `fee_conversion_salt` instead would let
    /// miden-client commit its own auth arg over this.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fee_faucet_id() -> AccountId {
        AccountId::from_hex("0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b").expect("valid faucet id")
    }

    #[test]
    fn accepts_an_approval_expiration_up_to_the_kernel_maximum() {
        let delta = NonZeroU32::new(MAX_APPROVAL_EXPIRATION_DELTA).expect("non-zero");
        let auth_args = multisig_auth_args(
            fee_faucet_id(),
            BlockNumber::from(10),
            Word::default(),
            Some(delta),
        )
        .expect("the maximum delta is accepted");
        assert_eq!(
            auth_args.approval_expiration_block_num(),
            Some(BlockNumber::from(10 + MAX_APPROVAL_EXPIRATION_DELTA))
        );
    }

    #[test]
    fn rejects_an_approval_expiration_the_auth_procedure_would_clamp() {
        let delta = NonZeroU32::new(MAX_APPROVAL_EXPIRATION_DELTA + 1).expect("non-zero");
        let error = multisig_auth_args(
            fee_faucet_id(),
            BlockNumber::from(10),
            Word::default(),
            Some(delta),
        )
        .expect_err("a delta past the kernel maximum is refused");
        assert!(
            error.to_string().contains("65535"),
            "the error names the limit: {error}"
        );
    }
}
