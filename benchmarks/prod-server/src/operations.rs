use anyhow::{Result, anyhow};
use guardian_client::ToJson;
use miden_protocol::account::AccountStoragePatch;
use miden_protocol::account::delta::AccountVaultDelta;
use miden_protocol::account::{AccountCodePatch, AccountDelta, AccountId};
use miden_protocol::block::BlockNumber;
use miden_protocol::transaction::{
    InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
};
use miden_protocol::{Felt, Word, ZERO};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OperationKind {
    GetCanonicalNonce,
    GetState,
    PushDelta,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GetCanonicalNonce => "get_canonical_nonce",
            Self::GetState => "get_state",
            Self::PushDelta => "push_delta",
        }
    }
}

pub fn create_delta_summary(account_id: &AccountId, nonce: u64) -> Result<TransactionSummary> {
    let account_delta = AccountDelta::new(
        *account_id,
        AccountStoragePatch::default(),
        AccountVaultDelta::default(),
        AccountCodePatch::default(),
        Felt::new_unchecked(nonce),
    )
    .map_err(|error| anyhow!("failed to build account delta: {error}"))?;
    Ok(TransactionSummary::new(
        account_delta,
        InputNotes::new(Vec::new())
            .map_err(|error| anyhow!("failed to build input notes: {error}"))?,
        RawOutputNotes::new(Vec::new())
            .map_err(|error| anyhow!("failed to build output notes: {error}"))?,
        BlockNumber::from(0),
        Word::from([ZERO; 4]),
        0,
        TransactionSummaryUserParams::new([ZERO; 6]),
    ))
}

pub fn create_delta_payload(account_id: &AccountId, nonce: u64) -> Result<Value> {
    Ok(create_delta_summary(account_id, nonce)?.to_json())
}

/// The proposal for a benchmark delta, as a client stores it before
/// collecting approvals: a multisig push is admitted only against a pending
/// proposal holding the threshold of verified approvals.
pub fn proposal_payload(summary: &TransactionSummary) -> Value {
    serde_json::json!({
        "tx_summary": summary.to_json(),
        "metadata": {
            "proposal_type": "benchmark",
            "bound_block_num": summary.block_number().as_u32(),
        },
    })
}
