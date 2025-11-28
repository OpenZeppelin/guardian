//! Error types for PSM client operations.

use thiserror::Error;

/// A Result type alias for PSM client operations.
pub type ClientResult<T> = Result<T, ClientError>;

/// Errors that can occur when using the PSM client.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Failed to establish connection to the PSM server.
    #[error("gRPC transport error: {0}")]
    Transport(#[from] tonic::transport::Error),

    /// The server returned a gRPC error status.
    #[error("gRPC status error: {0}")]
    Status(Box<tonic::Status>),

    /// The server returned an application-level error.
    #[error("Server returned error: {0}")]
    ServerError(String),

    /// Failed to serialize or deserialize JSON data.
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    /// The server response was invalid or unexpected.
    #[error("Invalid response: {0}")]
    InvalidResponse(String),
}

impl From<tonic::Status> for ClientError {
    fn from(status: tonic::Status) -> Self {
        ClientError::Status(Box::new(status))
    }
}
