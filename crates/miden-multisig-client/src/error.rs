//! Error types for the multisig client SDK.

use miden_protocol::account::AccountId;
use miden_protocol::note::NoteId;
use thiserror::Error;

use miden_client::rpc::{GrpcError, RpcError};

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

    /// Sync panicked due to corrupted local state (miden-client v0.12.x workaround).
    #[error("sync panicked (corrupted local state): {0}")]
    SyncPanicked(String),

    /// Transaction execution failed.
    #[error("transaction execution failed: {0}")]
    TransactionExecution(String),

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

    /// Returns the typed gRPC failure kind when this error originated from a Miden RPC request.
    pub fn miden_rpc_kind(&self) -> Option<&GrpcError> {
        match self {
            Self::MidenClientSource { source, .. } => rpc_kind_from_client_error(source),
            Self::MidenRpc(source) => rpc_kind(source),
            _ => None,
        }
    }

    /// Returns whether retrying the Miden operation can recover from this error.
    ///
    /// The typed status is the signal wherever one survives. Two shapes have no
    /// usable status and are decided on the rendered chain instead:
    ///
    ///   * `Unknown`, which the node returns both for a dropped connection and
    ///     for a genuine server fault -- only the former is worth another
    ///     attempt, so it must carry transport wording to qualify;
    ///   * errors that never become an `RpcError`. Note transport is the one
    ///     that matters: `Fetch notes failed: Status { code: Cancelled, ... }`
    ///     reaches here as a plain client error with no status to read.
    ///
    /// Measured on the 64-writer #317 leg, those two shapes plus `Cancelled`
    /// were 306 of 645 failures -- all idempotent reads and syncs, every one
    /// abandoned after a single attempt.
    pub fn is_transient_miden_rpc(&self) -> bool {
        match self.miden_rpc_kind() {
            Some(
                GrpcError::ResourceExhausted
                | GrpcError::DeadlineExceeded
                | GrpcError::Unavailable
                | GrpcError::Internal
                | GrpcError::Aborted
                // A client-side deadline on the request, not a verdict on it.
                | GrpcError::Cancelled,
            ) => true,
            Some(GrpcError::Unknown(_)) => self.carries_transport_failure(),
            Some(_) => false,
            None => self.carries_transport_failure(),
        }
    }

    /// Whether the error chain describes a connection that failed rather than a
    /// request the server refused.
    ///
    /// Kept deliberately narrow: it decides retries for errors carrying no
    /// status, so it must not match a server's considered rejection.
    fn carries_transport_failure(&self) -> bool {
        const TRANSPORT_SIGNALS: [&str; 6] = [
            "i/o timeout",
            "connection error",
            "transport error",
            "connection reset",
            "broken pipe",
            "timeout expired",
        ];

        let mut message = self.to_string().to_ascii_lowercase();
        let mut source = std::error::Error::source(self);
        while let Some(cause) = source {
            message.push(' ');
            message.push_str(&cause.to_string().to_ascii_lowercase());
            source = cause.source();
        }

        TRANSPORT_SIGNALS
            .iter()
            .any(|signal| message.contains(signal))
    }

    /// Returns whether the Miden node rejected the request because of resource pressure.
    pub fn is_miden_rate_limited(&self) -> bool {
        matches!(self.miden_rpc_kind(), Some(GrpcError::ResourceExhausted))
    }

    /// Returns whether a submission may be a duplicate of an earlier ambiguous attempt.
    pub fn is_miden_duplicate_submission(&self) -> bool {
        matches!(
            self.miden_rpc_kind(),
            Some(GrpcError::AlreadyExists | GrpcError::FailedPrecondition)
        )
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

fn rpc_kind(error: &RpcError) -> Option<&GrpcError> {
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

    fn request_error(kind: GrpcError) -> miden_client::ClientError {
        RpcError::RequestError {
            endpoint: RpcEndpoint::SyncChainMmr,
            error_kind: kind,
            endpoint_error: None,
            source: None,
        }
        .into()
    }

    #[test]
    fn miden_rpc_source_retains_typed_rate_limit() {
        let error = MultisigError::from(request_error(GrpcError::ResourceExhausted));

        assert!(error.is_transient_miden_rpc());
        assert!(error.is_miden_rate_limited());
        assert!(error.to_string().contains("sync_chain_mmr"));
    }

    #[test]
    fn permanent_miden_rpc_error_is_not_retryable() {
        let error = MultisigError::from(request_error(GrpcError::InvalidArgument));

        assert!(!error.is_transient_miden_rpc());
        assert!(!error.is_miden_rate_limited());
    }

    #[test]
    fn a_cancelled_request_is_retryable() {
        // A client-side deadline on the request, not a verdict on it. 73 of the
        // 645 failures on the #317 64-writer leg, every one abandoned at once.
        assert!(MultisigError::from(request_error(GrpcError::Cancelled)).is_transient_miden_rpc());
    }

    #[test]
    fn unknown_is_retryable_only_when_the_connection_failed() {
        // The node returns `Unknown` both for a dropped connection and for a
        // genuine fault. Retrying the first is right; retrying the second burns
        // the budget and delays the real error.
        let transport = MultisigError::from(request_error(GrpcError::Unknown(
            "transport error: code: 'Unknown error', message: \"transport error\", source: \
             tonic::transport::Error(Transport, hyper::Error(Io, Kind(TimedOut)))"
                .to_string(),
        )));
        assert!(
            matches!(transport.miden_rpc_kind(), Some(GrpcError::Unknown(_))),
            "the typed arm must be the one under test, not the status-less fallback"
        );
        assert!(transport.is_transient_miden_rpc());

        let fault = MultisigError::from(request_error(GrpcError::Unknown(
            "internal invariant violated".to_string(),
        )));
        assert!(!fault.is_transient_miden_rpc());
    }

    #[test]
    fn a_note_transport_failure_is_retryable_despite_carrying_no_status() {
        // Verbatim from `scale-20260729T230836Z-records.jsonl`, the single
        // largest failure family at 221 of 645. It never becomes an `RpcError`,
        // so `miden_rpc_kind()` is `None` and the typed path cannot see it.
        // The real error arrives as `MidenClientSource`, whose `message` is this
        // same chain flattened by `error_chain`, so the text read is identical.
        let error = MultisigError::MidenClient(
            "note transport error: note transport network error: Fetch notes failed: \
             Status { code: Cancelled, message: \"Timeout expired\", source: \
             Some(tonic::transport::Error(Transport, TimeoutExpired(()))) }"
                .to_string(),
        );

        assert!(error.miden_rpc_kind().is_none());
        assert!(error.is_transient_miden_rpc());
    }

    #[test]
    fn a_considered_rejection_is_never_treated_as_transport() {
        // The status-less path decides retries on wording, so it must not catch
        // a server that answered deliberately.
        for message in [
            "There's already a pending change for this account. Finish or cancel it first.",
            "Your session has expired. Please sign in again.",
            "account not found on chain",
        ] {
            assert!(
                !MultisigError::GuardianServer(message.to_string()).is_transient_miden_rpc(),
                "{message:?} must not be retried"
            );
        }
    }
}
