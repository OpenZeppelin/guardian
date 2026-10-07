//! Tells the network the suite runs over apart from the product it tests.
//!
//! The live profile drives a public Miden network and a remote prover. Neither
//! is under this repository's control, and both fail in ways that look exactly
//! like a scenario failing: a dropped connection mid-execution, a prover
//! deadline, a node that stops answering. Reporting those as product defects is
//! how a nightly schedule stops being read, so a failure whose evidence points
//! at the link is reported as environment-blocked instead.
//!
//! Status evidence is read the way [`guardian_shared::retry`] reads it:
//! permanent anywhere vetoes transient anywhere. The retry classifier's generic
//! wording fallback (`unavailable`, `timeout`, `cancelled`, ...) is not used.
//! A reason is the driver's own sentence wrapped around whatever it caught, and
//! those words turn up in both halves: `delta history unavailable: <a GUARDIAN
//! 500>` or a server message about a quorum timeout would otherwise stop
//! blocking the nightly. Only [`ENVIRONMENT_SIGNALS`], each specific to a
//! failing link, stands in for a missing status. The TypeScript driver applies
//! the same rule, and both are pinned to
//! `fixtures/qualification/environment-classification.json`.

use std::error::Error;

use guardian_shared::retry::{StructuredEvidence, flattened_grpc_evidence, http_evidence};

/// Wording that stands in for a missing status: `RPC_TRANSPORT_SIGNALS` (a node
/// rendering a dropped connection), the operating-system and runtime error
/// names that reach the driver as bare text through the TypeScript client's
/// WASM boundary, where the typed cause is already lost, tonic's rendering of
/// the transient gRPC codes, and the link-specific part of the retry fallback.
///
/// Every entry must be unambiguous evidence of a link failure. Guardian's own
/// error codes and messages travel in these same strings, so wording a
/// scenario could assert on (`network_error`, for one) or a bare `timeout` or
/// `unavailable` stays out deliberately.
pub const ENVIRONMENT_SIGNALS: [&str; 25] = [
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
    "the service is currently unavailable",
    "the operation was cancelled",
    "the deadline expired before the operation could complete",
    "deadline exceeded",
    "i/o timeout",
    "connection reset",
    "broken pipe",
    "bad gateway",
    "gateway timeout",
    "service unavailable",
];

/// Whether a failure reason is the environment failing under the suite.
#[must_use]
pub fn is_environmental(reason: &str) -> bool {
    let message = reason.to_ascii_lowercase();
    let mut transient = false;
    for evidence in [http_evidence(&message), flattened_grpc_evidence(&message)]
        .into_iter()
        .flatten()
    {
        match evidence {
            StructuredEvidence::Permanent => return false,
            StructuredEvidence::Transient => transient = true,
            StructuredEvidence::Indeterminate => {}
        }
    }
    transient
        || ENVIRONMENT_SIGNALS
            .iter()
            .any(|signal| message.contains(signal))
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
    use std::fmt;

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
