//! What block watch reads from the node: committed blocks, reduced to the
//! account updates it compares, delivered in sequence by a transport.

use async_trait::async_trait;

/// One committed block, reduced to what block watch needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedBlock {
    pub block_num: u32,
    /// The node's committed tip when it sent the block (never below the
    /// block itself), for the lag gauge.
    pub committed_chain_tip: u32,
    /// Every entry of the block's `updated_accounts`, in block order.
    pub updates: Vec<AccountUpdate>,
}

/// One account a block updated, and the commitment it ended the block at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountUpdate {
    /// Canonical hex account id, as `AccountId::to_hex` writes it.
    pub account_id: String,
    /// The final state commitment, hex-encoded like stored commitments.
    pub commitment: String,
    /// The update carries the public account patch.
    pub is_public: bool,
}

/// The transport a block source reads blocks over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// The node's `BlockSubscription` server stream.
    Stream,
    /// `GetBlockByNumber` calls driven by chain-tip reads.
    Poll,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stream => "stream",
            Self::Poll => "poll",
        }
    }
}

/// Why a block source ended. Every error ends the source instance it came
/// from; the feed decides whether to reopen it, wait, or switch transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockSourceError {
    /// The node answered or closed the stream with a gRPC status.
    Status { code: tonic::Code, message: String },
    /// The connection could not be made or broke without a status.
    Transport(String),
    /// A block arrived out of sequence: the source skipped or repeated one.
    OutOfSequence { expected: u32, got: u32 },
    /// The node sent a block without what every block has.
    Malformed(String),
    /// The node does not serve the block the feed needs next.
    Unavailable { block_num: u32 },
    /// The stream ended without a status.
    Ended,
}

impl std::fmt::Display for BlockSourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Status { code, message } => write!(f, "node status {code:?}: {message}"),
            Self::Transport(reason) => write!(f, "transport: {reason}"),
            Self::OutOfSequence { expected, got } => {
                write!(f, "block {got} out of sequence (expected {expected})")
            }
            Self::Malformed(reason) => write!(f, "malformed block: {reason}"),
            Self::Unavailable { block_num } => write!(f, "block {block_num} is not served"),
            Self::Ended => write!(f, "the stream ended without a status"),
        }
    }
}

impl BlockSourceError {
    /// The gRPC status code, if the node sent one.
    pub fn code(&self) -> Option<tonic::Code> {
        match self {
            Self::Status { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// The `reason` label of `guardian_block_watch_reconnects_total`: a
    /// closed set (one value per gRPC code, plus the non-status reasons).
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Status { code, .. } => status_reason(*code),
            Self::Transport(_) => "transport",
            Self::OutOfSequence { .. } => "out_of_sequence",
            Self::Malformed(_) => "malformed",
            Self::Unavailable { .. } => "unavailable_block",
            Self::Ended => "ended",
        }
    }
}

/// `status_<code>` for a gRPC code, from the code labels the server's own
/// gRPC metrics use.
fn status_reason(code: tonic::Code) -> &'static str {
    match crate::metrics::names::grpc_code_label(code as i32) {
        "ok" => "status_ok",
        "cancelled" => "status_cancelled",
        "invalid_argument" => "status_invalid_argument",
        "deadline_exceeded" => "status_deadline_exceeded",
        "not_found" => "status_not_found",
        "already_exists" => "status_already_exists",
        "permission_denied" => "status_permission_denied",
        "resource_exhausted" => "status_resource_exhausted",
        "failed_precondition" => "status_failed_precondition",
        "aborted" => "status_aborted",
        "out_of_range" => "status_out_of_range",
        "unimplemented" => "status_unimplemented",
        "internal" => "status_internal",
        "unavailable" => "status_unavailable",
        "data_loss" => "status_data_loss",
        "unauthenticated" => "status_unauthenticated",
        _ => "status_unknown",
    }
}

impl From<tonic::Status> for BlockSourceError {
    fn from(status: tonic::Status) -> Self {
        Self::Status {
            code: status.code(),
            message: status.message().to_string(),
        }
    }
}

/// A transport delivering committed blocks in sequence.
#[async_trait]
pub trait BlockSource: Send {
    /// The next block, exactly one past the previous one (the first is the
    /// block the source was opened at). An `Err` ends this source. Cancel
    /// safe: dropping the future loses no block.
    async fn next(&mut self) -> Result<ObservedBlock, BlockSourceError>;

    fn transport(&self) -> Transport;
}

/// Opens block sources at a given block, over either transport, and reads
/// the chain tip.
#[async_trait]
pub trait BlockSourceFactory: Send + Sync {
    async fn open_stream(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError>;

    async fn open_poll(&self, from: u32) -> Result<Box<dyn BlockSource>, BlockSourceError>;

    async fn chain_tip(&self) -> Result<u32, BlockSourceError>;

    /// The genesis commitment of the node's chain, when it reports one: a
    /// different one than the cursor recorded means the chain was reset.
    async fn genesis(&self) -> Result<Option<String>, BlockSourceError>;

    /// Whether the node serves `block_num` (a block at or below its tip it
    /// no longer serves cannot be replayed).
    async fn serves_block(&self, block_num: u32) -> Result<bool, BlockSourceError>;

    async fn open(
        &self,
        transport: Transport,
        from: u32,
    ) -> Result<Box<dyn BlockSource>, BlockSourceError> {
        match transport {
            Transport::Stream => self.open_stream(from).await,
            Transport::Poll => self.open_poll(from).await,
        }
    }
}

#[cfg(all(test, not(any(feature = "integration", feature = "e2e"))))]
mod tests {
    use super::*;

    #[test]
    fn every_reason_label_is_a_closed_snake_case_value() {
        let codes = [
            tonic::Code::Ok,
            tonic::Code::Cancelled,
            tonic::Code::Unknown,
            tonic::Code::InvalidArgument,
            tonic::Code::DeadlineExceeded,
            tonic::Code::NotFound,
            tonic::Code::AlreadyExists,
            tonic::Code::PermissionDenied,
            tonic::Code::ResourceExhausted,
            tonic::Code::FailedPrecondition,
            tonic::Code::Aborted,
            tonic::Code::OutOfRange,
            tonic::Code::Unimplemented,
            tonic::Code::Internal,
            tonic::Code::Unavailable,
            tonic::Code::DataLoss,
            tonic::Code::Unauthenticated,
        ];
        let mut seen = std::collections::HashSet::new();
        for code in codes {
            let reason = BlockSourceError::Status {
                code,
                message: String::new(),
            }
            .reason();
            assert!(reason.starts_with("status_"), "{reason}");
            assert!(seen.insert(reason), "{reason} used twice");
        }
        assert_eq!(
            BlockSourceError::Status {
                code: tonic::Code::ResourceExhausted,
                message: String::new()
            }
            .reason(),
            "status_resource_exhausted"
        );
        for error in [
            BlockSourceError::Transport(String::new()),
            BlockSourceError::OutOfSequence {
                expected: 1,
                got: 2,
            },
            BlockSourceError::Malformed(String::new()),
            BlockSourceError::Unavailable { block_num: 1 },
            BlockSourceError::Ended,
        ] {
            assert!(
                error
                    .reason()
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_')
            );
        }
    }
}
