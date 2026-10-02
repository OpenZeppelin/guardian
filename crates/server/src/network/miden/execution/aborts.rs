//! Failure causes read from the MASM assertion a transaction aborted on. Upstream exports its
//! error constants only for testing, so the messages are pinned here and checked against those
//! constants in tests.

use miden_processor::ExecutionError;
use miden_processor::operation::OperationError;
use miden_protocol::Felt;
use miden_protocol::assembly::mast::error_code_from_msg;
use miden_tx::TransactionExecutorError;

pub(super) const APPROVAL_EXPIRED: &str =
    "the multisig approval expired at or before the transaction reference block";
pub(super) const VAULT_SHORTFALL: &str = "failed to remove the fungible asset from the vault since the amount of the asset in the vault is less than the amount to remove";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Abort {
    ApprovalExpired,
    VaultShortfall,
}

impl Abort {
    pub(super) fn of(error: &TransactionExecutorError) -> Option<Self> {
        let TransactionExecutorError::TransactionProgramExecutionFailed(
            ExecutionError::OperationError {
                err: OperationError::FailedAssertion { err_code, .. },
                ..
            },
        ) = error
        else {
            return None;
        };
        [Self::ApprovalExpired, Self::VaultShortfall]
            .into_iter()
            .find(|abort| abort.code() == *err_code)
    }

    fn code(self) -> Felt {
        error_code_from_msg(match self {
            Self::ApprovalExpired => APPROVAL_EXPIRED,
            Self::VaultShortfall => VAULT_SHORTFALL,
        })
    }
}
