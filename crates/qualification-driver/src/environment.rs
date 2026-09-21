//! Tells the network the suite runs over apart from the product it tests.
//!
//! The live profile drives a public Miden network and a remote prover. Neither
//! is under this repository's control, and both fail in ways that look exactly
//! like a scenario failing: a dropped connection mid-execution, a prover
//! deadline, a node that stops answering. Reporting those as product defects is
//! how a nightly schedule stops being read, so a failure whose evidence points
//! at the link is reported as environment-blocked instead.
//!
//! The rule is [`guardian_shared::retry`]'s transient classifier, unchanged:
//! permanent status evidence anywhere vetoes transient evidence anywhere, and
//! the wording fallback is consulted only when no link carried a status. The
//! TypeScript driver applies the same rule through its own mirror of that
//! classifier, and both are pinned to
//! `fixtures/qualification/environment-classification.json`.

use std::error::Error;
use std::fmt;

use guardian_shared::retry::{StructuredEvidence, is_transient_error_with};

/// Transport wording the shared fallback does not carry: `RPC_TRANSPORT_SIGNALS`
/// (a node rendering a dropped connection) plus the operating-system and
/// runtime error names that reach the driver as bare text through the
/// TypeScript client's WASM boundary, where the typed cause is already lost.
///
/// Every entry must be unambiguous evidence of a link failure. Guardian's own
/// error codes travel in these same strings, so wording a scenario could assert
/// on (`network_error`, for one) stays out deliberately.
pub const ENVIRONMENT_SIGNALS: [&str; 15] = [
    "connection error",
    "transport error",
    "timed out",
    "etimedout",
    "econnreset",
    "econnrefused",
    "econnaborted",
    "ehostunreach",
    "enetunreach",
    "epipe",
    "eai_again",
    "socket hang up",
    "fetch failed",
    "err_http2_stream_error",
    "invalid content type: application/grpc",
];

/// A failure reason, flattened to text, so the shared classifier can walk it.
///
/// By the time a reason reaches the report it is already a string: the driver
/// composes what it asserted with what it caught. Wrapping it back into an
/// error keeps one classifier for both SDKs rather than a second, subtly
/// different, string matcher living here.
#[derive(Debug)]
struct Reason(String);

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for Reason {}

/// Whether a failure reason is the environment failing under the suite.
#[must_use]
pub fn is_environmental(reason: &str) -> bool {
    is_transient_error_with(
        &Reason(reason.to_string()),
        |_| StructuredEvidence::Indeterminate,
        &ENVIRONMENT_SIGNALS,
    )
}

/// Renders an error together with everything that caused it.
///
/// Several of the Miden client's errors print a summary and keep the cause on
/// the source chain, so a reason built from `{error}` alone loses the very
/// evidence [`is_environmental`] reads. A funding transfer that reported only
/// `transaction proving failed` is the case this exists for.
#[must_use]
pub fn error_chain(error: &(dyn Error + 'static)) -> String {
    let mut rendered = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !rendered.contains(&text) {
            rendered.push_str(": ");
            rendered.push_str(&text);
        }
        source = cause.source();
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Fixtures {
        signals: Vec<String>,
        reasons: Vec<ReasonFixture>,
    }

    #[derive(Deserialize)]
    struct ReasonFixture {
        name: String,
        reason: String,
        environmental: bool,
    }

    fn fixtures() -> Fixtures {
        serde_json::from_str(include_str!(
            "../../../fixtures/qualification/environment-classification.json"
        ))
        .expect("the environment classification fixtures must parse")
    }

    #[test]
    fn classification_vectors_match_contract() {
        for fixture in fixtures().reasons {
            assert_eq!(
                is_environmental(&fixture.reason),
                fixture.environmental,
                "fixture: {}",
                fixture.name
            );
        }
    }

    /// The TypeScript driver reads the same list out of the same file. A signal
    /// added on one side and not the other is the drift this catches.
    #[test]
    fn the_signal_list_matches_the_shared_fixture() {
        assert_eq!(fixtures().signals, ENVIRONMENT_SIGNALS);
    }

    #[test]
    fn a_chain_renders_every_cause() {
        #[derive(Debug)]
        struct Layer {
            message: &'static str,
            source: Option<Box<Layer>>,
        }

        impl fmt::Display for Layer {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.message)
            }
        }

        impl Error for Layer {
            fn source(&self) -> Option<&(dyn Error + 'static)> {
                self.source.as_deref().map(|source| source as _)
            }
        }

        let error = Layer {
            message: "transaction proving failed",
            source: Some(Box::new(Layer {
                message: "transport error: connection error",
                source: None,
            })),
        };
        assert_eq!(
            error_chain(&error),
            "transaction proving failed: transport error: connection error"
        );
        assert!(is_environmental(&error_chain(&error)));
        assert!(!is_environmental(&error.to_string()));
    }
}
