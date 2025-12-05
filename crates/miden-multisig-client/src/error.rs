//! Error types for the multisig client SDK.

use miden_objects::account::AccountId;
use thiserror::Error;

/// Result type alias for multisig operations.
pub type Result<T> = std::result::Result<T, MultisigError>;

/// Errors that can occur during multisig operations.
#[derive(Debug, Error)]
pub enum MultisigError {
    /// Account not found in local cache.
    #[error("account not found: {0}")]
    AccountNotFound(AccountId),

    /// Proposal not found.
    #[error("proposal not found: {0}")]
    ProposalNotFound(String),

    /// PSM connection error.
    #[error("PSM connection error: {0}")]
    PsmConnection(String),

    /// PSM server returned an error.
    #[error("PSM server error: {0}")]
    PsmServer(String),

    /// Miden client error.
    #[error("miden client error: {0}")]
    MidenClient(String),

    /// Sync panicked due to corrupted local state (miden-client v0.12.x workaround).
    #[error("sync panicked (corrupted local state): {0}")]
    SyncPanicked(String),

    /// Transaction execution failed.
    #[error("transaction execution failed: {0}")]
    TransactionExecution(String),

    /// Invalid configuration.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Signature error.
    #[error("signature error: {0}")]
    Signature(String),

    /// Serialization/deserialization error.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// User is not a cosigner for this account.
    #[error("not a cosigner for this account")]
    NotCosigner,

    /// User has already signed this proposal.
    #[error("already signed this proposal")]
    AlreadySigned,

    /// Proposal does not have enough signatures for finalization.
    #[error("proposal not ready: need {required} signatures, have {collected}")]
    ProposalNotReady { required: usize, collected: usize },

    /// Key manager not configured.
    #[error("key manager not configured")]
    NoKeyManager,

    /// Missing required configuration.
    #[error("missing required configuration: {0}")]
    MissingConfig(String),

    /// Hex decoding error.
    #[error("hex decode error: {0}")]
    HexDecode(String),

    /// Account storage error.
    #[error("account storage error: {0}")]
    AccountStorage(String),

    /// Transaction unexpected success (expected Unauthorized).
    #[error("transaction executed successfully when failure was expected")]
    UnexpectedSuccess,

    /// Unknown transaction type encountered during parsing.
    #[error("unknown transaction type: {0}")]
    UnknownTransactionType(String),

    /// Invalid filter configuration.
    #[error("invalid filter: {0}")]
    InvalidFilter(String),
}

impl From<private_state_manager_client::ClientError> for MultisigError {
    fn from(err: private_state_manager_client::ClientError) -> Self {
        MultisigError::PsmServer(err.to_string())
    }
}

impl From<miden_client::ClientError> for MultisigError {
    fn from(err: miden_client::ClientError) -> Self {
        MultisigError::MidenClient(err.to_string())
    }
}

impl From<miden_client::transaction::TransactionRequestError> for MultisigError {
    fn from(err: miden_client::transaction::TransactionRequestError) -> Self {
        MultisigError::TransactionExecution(err.to_string())
    }
}

impl From<miden_client::transaction::TransactionExecutorError> for MultisigError {
    fn from(err: miden_client::transaction::TransactionExecutorError) -> Self {
        MultisigError::TransactionExecution(err.to_string())
    }
}
