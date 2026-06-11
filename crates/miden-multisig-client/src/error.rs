//! Error types for the multisig client SDK.

use miden_protocol::account::AccountId;
use miden_protocol::note::NoteId;
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

    /// GUARDIAN connection error.
    #[error("GUARDIAN connection error: {0}")]
    GuardianConnection(String),

    /// GUARDIAN server returned an error.
    #[error("GUARDIAN server error: {0}")]
    GuardianServer(String),

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

    /// Signer not configured.
    #[error("signer not configured")]
    NoSigner,

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

    /// Retained for backward compatibility; no longer produced. Unmodeled
    /// proposal types now parse into `TransactionType::Custom` (issue #266), and
    /// build/execute failures surface as `UnsupportedTransactionType`.
    #[error("unknown transaction type: {0}")]
    UnknownTransactionType(String),

    /// A custom/unmodeled proposal type cannot be built or executed by the
    /// generic SDK (issue #266). It can still be parsed, signed, and exported.
    #[error("unsupported transaction type for this operation: {0}")]
    UnsupportedTransactionType(String),

    /// Invalid filter configuration.
    #[error("invalid filter: {0}")]
    InvalidFilter(String),

    /// Transaction type is not supported in offline mode without GUARDIAN.
    #[error("offline mode only supports SwitchGuardian transactions, got: {0}")]
    OfflineUnsupportedTransaction(String),

    /// consume_notes v2 metadata: embedded `notes` array does not match
    /// declared `note_ids` (length mismatch or per-index ID mismatch).
    #[error("consume_notes metadata note binding mismatch: {0}")]
    NoteBindingMismatch(String),

    /// consume_notes metadata has an unrecognized version, or is v1 on a
    /// cut-over build that no longer supports the legacy path.
    #[error("unsupported consume_notes metadata version: {found:?}")]
    UnsupportedMetadataVersion { found: Option<u32> },

    /// consume_notes v2 metadata exceeds the per-proposal size cap.
    #[error(
        "consume_notes metadata exceeds size limit: limit={limit} bytes, actual={actual} bytes"
    )]
    ConsumeNotesMetadataOversize { limit: usize, actual: usize },

    /// consume_notes v1 verification path: the cosigner's local Miden
    /// store does not contain the referenced note. Not reachable on v2.
    #[error("consume_notes legacy verification: note not found in local store: {note_id}")]
    LegacyConsumeNotesNoteMissing { note_id: NoteId },
}

impl MultisigError {
    /// Stable, machine-readable identifier for cross-SDK error parity
    /// per spec FR-021 / FR-022. Only consume_notes-feature errors are
    /// pinned here for now; broader taxonomy work is out of scope.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::NoteBindingMismatch(_) => Some("consume_notes_note_binding_mismatch"),
            Self::UnsupportedMetadataVersion { .. } => {
                Some("consume_notes_unsupported_metadata_version")
            }
            Self::ConsumeNotesMetadataOversize { .. } => Some("consume_notes_metadata_oversize"),
            Self::LegacyConsumeNotesNoteMissing { .. } => Some("consume_notes_legacy_note_missing"),
            Self::UnsupportedTransactionType(_) => Some("unsupported_transaction_type"),
            _ => None,
        }
    }
}

impl From<guardian_client::ClientError> for MultisigError {
    fn from(err: guardian_client::ClientError) -> Self {
        MultisigError::GuardianServer(err.to_string())
    }
}

/// Flattens an error's full `source()` chain into a single string so that
/// callers see the underlying cause (e.g. the gRPC status behind a terse
/// "RPC error") instead of only the outermost `Display`.
pub(crate) fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

impl From<miden_client::ClientError> for MultisigError {
    fn from(err: miden_client::ClientError) -> Self {
        MultisigError::MidenClient(error_chain(&err))
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
