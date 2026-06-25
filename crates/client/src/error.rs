//! Error types for GUARDIAN client operations.

use thiserror::Error;

/// A Result type alias for GUARDIAN client operations.
pub type ClientResult<T> = Result<T, ClientError>;

/// Errors that can occur when using the GUARDIAN client.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Failed to establish connection to the GUARDIAN server.
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

impl ClientError {
    /// Parse the structured `{ code, message, meta }` object Guardian attaches
    /// to a gRPC `Status.details` (feature `009-human-readable-errors`).
    /// Returns `None` for non-Status errors or Status errors without details.
    fn guardian_error_details(&self) -> Option<serde_json::Value> {
        match self {
            ClientError::Status(status) => {
                let details = status.details();
                if details.is_empty() {
                    return None;
                }
                serde_json::from_slice(details).ok()
            }
            _ => None,
        }
    }

    /// Stable, machine-readable Guardian error code (e.g. `account_paused`),
    /// when this error originated from a Guardian gRPC `Status`. Branch on
    /// this rather than on the message text.
    pub fn guardian_code(&self) -> Option<String> {
        self.guardian_error_details()?
            .get("code")?
            .as_str()
            .map(str::to_owned)
    }

    /// Short, end-user-safe message safe to show in a wallet UI, when this
    /// error originated from a Guardian gRPC `Status`. Falls back to the
    /// `Status.message` (also the user-safe message) for Status errors
    /// without a details object.
    pub fn user_message(&self) -> Option<String> {
        if let Some(message) = self
            .guardian_error_details()
            .and_then(|d| d.get("message")?.as_str().map(str::to_owned))
        {
            return Some(message);
        }
        match self {
            ClientError::Status(status) => Some(status.message().to_owned()),
            _ => None,
        }
    }

    /// The structured `meta` block (`retryable`, `retry_after_secs`, …) from a
    /// Guardian gRPC `Status`, when present.
    pub fn guardian_meta(&self) -> Option<serde_json::Value> {
        self.guardian_error_details()?.get("meta").cloned()
    }

    /// Whether this error means "the requested record does not exist". Handles
    /// both the gRPC `Status` path (gRPC `NotFound`, feature 009) and the
    /// legacy in-band `ServerError` message. Callers use this instead of
    /// substring-matching the message text.
    pub fn is_not_found(&self) -> bool {
        match self {
            ClientError::Status(status) => status.code() == tonic::Code::NotFound,
            ClientError::ServerError(msg) => msg.contains("not found"),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guardian_status() -> tonic::Status {
        // Mirrors the object the server attaches to Status.details (feature 009).
        let details = serde_json::json!({
            "code": "account_paused",
            "message": "This account is paused and can't approve transactions right now.",
            "meta": { "retryable": false }
        })
        .to_string()
        .into_bytes();
        tonic::Status::with_details(
            tonic::Code::FailedPrecondition,
            "This account is paused and can't approve transactions right now.",
            details.into(),
        )
    }

    #[test]
    fn accessors_parse_guardian_status_details() {
        let err: ClientError = guardian_status().into();
        assert_eq!(err.guardian_code().as_deref(), Some("account_paused"));
        assert_eq!(
            err.user_message().as_deref(),
            Some("This account is paused and can't approve transactions right now.")
        );
        assert_eq!(err.guardian_meta().unwrap()["retryable"], false);
        assert!(!err.is_not_found());
    }

    #[test]
    fn user_message_falls_back_to_status_message_without_details() {
        let err: ClientError = tonic::Status::new(tonic::Code::Unavailable, "fallback msg").into();
        assert_eq!(err.guardian_code(), None);
        assert_eq!(err.user_message().as_deref(), Some("fallback msg"));
    }

    #[test]
    fn is_not_found_detects_grpc_not_found_and_legacy_message() {
        let status: ClientError = tonic::Status::new(tonic::Code::NotFound, "x").into();
        assert!(status.is_not_found());
        assert!(ClientError::ServerError("Delta not found for account".into()).is_not_found());
        assert!(!ClientError::ServerError("boom".into()).is_not_found());
    }
}
