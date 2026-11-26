//! The fee a transaction paid after its signers signed the summary. It is recorded in the delta
//! payload next to the summary, because the summary's account delta does not withdraw it.

use std::borrow::Cow;

use miden_protocol::account::delta::{AccountVaultDelta, AssetDelta, AssetDeltaOperation};
use miden_protocol::account::{AccountDelta, AccountId};
use miden_protocol::asset::{Asset, FungibleAsset};
use miden_protocol::transaction::TransactionSummary;
use serde_json::{Value, json};

const PAID_FEE_KEY: &str = "paid_fee";

/// `payload` with `fee` recorded as paid after its summary.
pub(crate) fn with_paid_fee(payload: &Value, fee: &FungibleAsset) -> Value {
    let mut payload = payload.clone();
    payload[PAID_FEE_KEY] = json!({
        "faucet_id": fee.faucet_id().to_hex(),
        "amount": fee.amount().as_u64().to_string(),
    });
    payload
}

/// The fee recorded in `payload`, if any.
pub(crate) fn paid_fee(payload: &Value) -> Result<Option<FungibleAsset>, String> {
    let Some(fee) = payload.get(PAID_FEE_KEY) else {
        return Ok(None);
    };
    let faucet_id = fee
        .get("faucet_id")
        .and_then(Value::as_str)
        .ok_or("paid fee without a faucet id")?;
    let amount = fee
        .get("amount")
        .and_then(Value::as_str)
        .ok_or("paid fee without an amount")?;
    let faucet_id =
        AccountId::from_hex(faucet_id).map_err(|e| format!("invalid paid fee faucet id: {e}"))?;
    let amount = amount
        .parse::<u64>()
        .map_err(|e| format!("invalid paid fee amount: {e}"))?;
    FungibleAsset::new(faucet_id, amount)
        .map(Some)
        .map_err(|e| format!("invalid paid fee: {e}"))
}

/// The account delta the transaction applied: the summary's, plus the withdrawal of `fee`.
pub(crate) fn settled_account_delta(
    summary: &TransactionSummary,
    fee: Option<FungibleAsset>,
) -> Result<Cow<'_, AccountDelta>, String> {
    let delta = summary.account_delta();
    let Some(fee) = fee else {
        return Ok(Cow::Borrowed(delta));
    };
    let withdrawal = AccountVaultDelta::new([AssetDelta::new(
        AssetDeltaOperation::Remove,
        Asset::from(fee),
    )])
    .map_err(|e| format!("failed to build the paid fee withdrawal: {e}"))?;
    AccountDelta::new(
        delta.id(),
        delta.storage().clone(),
        super::merge_vault_deltas([delta.vault(), &withdrawal])?,
        delta.code().clone(),
        delta.nonce_delta(),
    )
    .map(Cow::Owned)
    .map_err(|e| format!("failed to build the settled account delta: {e}"))
}
