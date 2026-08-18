use anyhow::{Result, anyhow};
use guardian_client::ToJson;
use miden_protocol::account::AccountStoragePatch;
use miden_protocol::account::delta::AccountVaultDelta;
use miden_protocol::account::{AccountDelta, AccountId};
use miden_protocol::transaction::{
    InputNotes, RawOutputNotes, TransactionSummary, TransactionSummaryUserParams,
};
use miden_protocol::{Felt, Word, ZERO};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OperationKind {
    GetState,
    PushDelta,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GetState => "get_state",
            Self::PushDelta => "push_delta",
        }
    }
}

pub fn create_delta_payload(account_id: &AccountId, nonce: u64) -> Result<Value> {
    let account_delta = AccountDelta::new(
        *account_id,
        AccountStoragePatch::default(),
        AccountVaultDelta::default(),
        None,
        Felt::new_unchecked(nonce),
    )
    .map_err(|error| anyhow!("failed to build account delta: {error}"))?;
    let tx_summary = TransactionSummary::new(
        account_delta,
        InputNotes::new(Vec::new())
            .map_err(|error| anyhow!("failed to build input notes: {error}"))?,
        RawOutputNotes::new(Vec::new())
            .map_err(|error| anyhow!("failed to build output notes: {error}"))?,
        Word::from([ZERO; 4]),
        0,
        TransactionSummaryUserParams::new([ZERO; 7]),
    );
    Ok(tx_summary.to_json())
}
