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
    /// error originated from a Guardian gRPC `Status` carrying the structured
    /// `{ code, message, meta }` details object. Returns `None` when the
    /// details object is absent — a bare `Status.message` may be
    /// developer-facing text (e.g. pre-service auth-metadata validation), so
    /// callers substitute their own generic safe message instead.
    pub fn user_message(&self) -> Option<String> {
        self.guardian_error_details()
            .and_then(|d| d.get("message")?.as_str().map(str::to_owned))
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

    /// Whether the server marked this error safe to retry: `meta.retryable`
    /// from the structured details, falling back to the status-code class
    /// (`ResourceExhausted` rejections happen before any handler runs).
    pub fn is_retryable(&self) -> bool {
        if let Some(meta) = self.guardian_meta()
            && let Some(retryable) = meta.get("retryable").and_then(|v| v.as_bool())
        {
            return retryable;
        }
        matches!(
            self,
            ClientError::Status(status) if status.code() == tonic::Code::ResourceExhausted
        )
    }

    /// Server-provided backoff hint: the `retry-after` status metadata
    /// (decimal seconds), falling back to `meta.retry_after_secs`.
    /// Unparseable values mean no hint; classification is unaffected.
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        if let ClientError::Status(status) = self
            && let Some(value) = status.metadata().get("retry-after")
            && let Ok(text) = value.to_str()
            && let Ok(secs) = text.trim().parse::<u64>()
        {
            return Some(std::time::Duration::from_secs(secs));
        }
        let secs = self.guardian_meta()?.get("retry_after_secs")?.as_u64()?;
        Some(std::time::Duration::from_secs(secs))
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
    fn user_message_is_none_without_structured_details() {
        // A bare Status.message may be developer-facing (e.g. pre-service
        // auth-metadata validation); it must not be surfaced as user-safe.
        let err: ClientError =
            tonic::Status::new(tonic::Code::Unavailable, "raw internal detail").into();
        assert_eq!(err.guardian_code(), None);
        assert_eq!(err.user_message(), None);
    }

    #[test]
    fn is_not_found_detects_grpc_not_found_and_legacy_message() {
        let status: ClientError = tonic::Status::new(tonic::Code::NotFound, "x").into();
        assert!(status.is_not_found());
        assert!(ClientError::ServerError("Delta not found for account".into()).is_not_found());
        assert!(!ClientError::ServerError("boom".into()).is_not_found());
    }

    #[test]
    fn retry_classification_matches_shared_fixture() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/guardian-client/rate-limit-policy.json"
        );
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();

        for case in fixture["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let grpc = &case["grpc"];
            let code = tonic::Code::from_i32(grpc["code"].as_i64().unwrap() as i32);
            let mut status = if case["body"].is_null() {
                tonic::Status::new(code, "plain failure")
            } else {
                tonic::Status::with_details(
                    code,
                    "test",
                    case["body"].to_string().into_bytes().into(),
                )
            };
            if let Some(hint) = grpc["retryAfterMetadata"].as_str() {
                status.metadata_mut().insert(
                    "retry-after",
                    hint.parse().expect("fixture hints are ASCII"),
                );
            }

            let err: ClientError = status.into();
            assert_eq!(
                err.is_retryable(),
                case["expected"]["retryable"].as_bool().unwrap(),
                "retryable mismatch: {name}"
            );
            assert_eq!(
                err.retry_after(),
                case["expected"]["retryAfterSecs"]
                    .as_u64()
                    .map(std::time::Duration::from_secs),
                "retry hint mismatch: {name}"
            );
        }
    }
}
