//! Error types for the multisig client SDK.

use miden_client::rpc::{GrpcError, RpcError};
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

    /// Miden client error retaining the concrete source and its RPC status.
    #[error("miden client error: {message}")]
    MidenClientSource {
        message: String,
        #[source]
        source: Box<miden_client::ClientError>,
    },

    /// Direct Miden RPC error retaining its endpoint and typed gRPC status.
    #[error("miden RPC error: {0}")]
    MidenRpc(#[source] Box<RpcError>),

    /// Direct Miden RPC error with call-site context, retaining the typed
    /// status.
    #[error("miden RPC error: {message}")]
    MidenRpcSource {
        message: String,
        #[source]
        source: Box<RpcError>,
    },

    /// Sync panicked due to corrupted local state (miden-client v0.12.x workaround).
    #[error("sync panicked (corrupted local state): {0}")]
    SyncPanicked(String),

    /// Transaction execution failed.
    #[error("transaction execution failed: {0}")]
    TransactionExecution(String),

    /// Transaction-stage Miden client failure retaining the typed source.
    /// Never used for retry decisions — execution stages run once — but the
    /// typed status survives for caller diagnostics.
    #[error("transaction execution failed: {message}")]
    TransactionExecutionSource {
        message: String,
        #[source]
        source: Box<miden_client::ClientError>,
    },

    /// Invalid configuration.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// Invalid custom transaction prover URL.
    #[error("invalid prover URL: {0}")]
    InvalidProverUrl(String),

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

    /// Wraps a Miden client failure with call-site context while retaining the
    /// typed source for classification.
    pub(crate) fn miden_client_with_context(
        context: impl AsRef<str>,
        err: miden_client::ClientError,
    ) -> Self {
        MultisigError::MidenClientSource {
            message: format!("{}: {}", context.as_ref(), error_chain(&err)),
            source: Box::new(err),
        }
    }

    /// Wraps a direct Miden RPC failure with call-site context while
    /// retaining the typed source for classification.
    pub(crate) fn miden_rpc_with_context(context: impl AsRef<str>, err: RpcError) -> Self {
        MultisigError::MidenRpcSource {
            message: format!("{}: {}", context.as_ref(), error_chain(&err)),
            source: Box::new(err),
        }
    }

    /// Wraps a transaction-stage Miden client failure with call-site context
    /// while retaining the typed source.
    pub(crate) fn transaction_execution_with_context(
        context: impl AsRef<str>,
        err: miden_client::ClientError,
    ) -> Self {
        MultisigError::TransactionExecutionSource {
            message: format!("{}: {}", context.as_ref(), error_chain(&err)),
            source: Box::new(err),
        }
    }

    /// Returns the typed gRPC failure kind when this error originated from a
    /// Miden RPC request.
    pub fn miden_rpc_kind(&self) -> Option<&GrpcError> {
        match self {
            Self::MidenClientSource { source, .. } => rpc_kind_from_client_error(source),
            Self::MidenRpc(source) => rpc_kind(source),
            Self::MidenRpcSource { source, .. } => rpc_kind(source),
            Self::TransactionExecutionSource { source, .. } => rpc_kind_from_client_error(source),
            _ => None,
        }
    }
}

fn rpc_kind_from_client_error(error: &miden_client::ClientError) -> Option<&GrpcError> {
    match error {
        miden_client::ClientError::RpcError(error) => rpc_kind(error),
        miden_client::ClientError::ApplyTransactionAfterSubmitFailed { source, .. } => {
            rpc_kind_from_client_error(source)
        }
        _ => None,
    }
}

pub(crate) fn rpc_kind(error: &RpcError) -> Option<&GrpcError> {
    match error {
        RpcError::RequestError { error_kind, .. } => Some(error_kind),
        _ => None,
    }
}

impl From<guardian_client::ClientError> for MultisigError {
    fn from(err: guardian_client::ClientError) -> Self {
        MultisigError::GuardianServer(err.to_string())
    }
}

/// Flattens an error's full `source()` chain into one string so callers see the underlying cause
/// (e.g. the gRPC status behind a terse "RPC error"), not just the outermost `Display`.
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
        MultisigError::MidenClientSource {
            message: error_chain(&err),
            source: Box::new(err),
        }
    }
}

impl From<RpcError> for MultisigError {
    fn from(err: RpcError) -> Self {
        MultisigError::MidenRpc(Box::new(err))
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

#[cfg(test)]
mod tests {
    use miden_client::rpc::RpcEndpoint;

    use super::*;

    pub(crate) fn request_error(kind: GrpcError) -> miden_client::ClientError {
        RpcError::RequestError {
            endpoint: RpcEndpoint::SyncChainMmr,
            error_kind: kind,
            endpoint_error: None,
            source: None,
        }
        .into()
    }

    #[test]
    fn miden_client_conversion_retains_typed_rpc_kind() {
        let error = MultisigError::from(request_error(GrpcError::ResourceExhausted));

        assert!(matches!(
            error.miden_rpc_kind(),
            Some(GrpcError::ResourceExhausted)
        ));
        assert!(error.to_string().contains("sync_chain_mmr"));
    }

    #[test]
    fn direct_rpc_conversion_retains_typed_rpc_kind() {
        let error = MultisigError::from(RpcError::RequestError {
            endpoint: RpcEndpoint::GetAccount,
            error_kind: GrpcError::Unavailable,
            endpoint_error: None,
            source: None,
        });

        assert!(matches!(
            error.miden_rpc_kind(),
            Some(GrpcError::Unavailable)
        ));
    }

    #[test]
    fn context_wrapper_keeps_call_site_detail_and_typed_source() {
        let error = MultisigError::miden_client_with_context(
            "failed to fetch on-chain commitment for account 0xabc",
            request_error(GrpcError::Unavailable),
        );

        assert!(error.to_string().contains("account 0xabc"));
        assert!(matches!(
            error.miden_rpc_kind(),
            Some(GrpcError::Unavailable)
        ));
    }

    #[test]
    fn errors_without_rpc_origin_have_no_rpc_kind() {
        assert!(
            MultisigError::GuardianServer("There's already a pending change".to_string())
                .miden_rpc_kind()
                .is_none()
        );
        assert!(
            MultisigError::MidenClient("account not found on chain".to_string())
                .miden_rpc_kind()
                .is_none()
        );
    }

    #[test]
    fn rpc_context_wrapper_keeps_call_site_detail_and_typed_source() {
        let error = MultisigError::miden_rpc_with_context(
            "failed to fetch on-chain commitment for account 0xabc",
            RpcError::RequestError {
                endpoint: RpcEndpoint::GetAccount,
                error_kind: GrpcError::ResourceExhausted,
                endpoint_error: None,
                source: None,
            },
        );

        assert!(error.to_string().contains("account 0xabc"));
        assert!(matches!(
            error.miden_rpc_kind(),
            Some(GrpcError::ResourceExhausted)
        ));
    }

    #[test]
    fn execution_context_wrapper_keeps_typed_source() {
        let error = MultisigError::transaction_execution_with_context(
            "transaction submission failed",
            request_error(GrpcError::Unavailable),
        );

        assert!(error.to_string().contains("transaction submission failed"));
        assert!(matches!(
            error.miden_rpc_kind(),
            Some(GrpcError::Unavailable)
        ));
    }
}
