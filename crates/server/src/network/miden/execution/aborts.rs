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
pub(super) const FEE_INSUFFICIENT_BALANCE: &str =
    "native account vault holds less of the fee asset than the fee to pay";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Abort {
    ApprovalExpired,
    FeeInsufficientBalance,
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
        [Self::ApprovalExpired, Self::FeeInsufficientBalance]
            .into_iter()
            .find(|abort| abort.code() == *err_code)
    }

    fn code(self) -> Felt {
        error_code_from_msg(match self {
            Self::ApprovalExpired => APPROVAL_EXPIRED,
            Self::FeeInsufficientBalance => FEE_INSUFFICIENT_BALANCE,
        })
    }
}
