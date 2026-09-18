use serde::{Deserialize, Serialize};

/// A workaround the harness had to apply that a consumer installing the same
/// artifact would also need. Recording it keeps a published-pairing pass from
/// speaking for something a consumer cannot actually do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumerFinding {
    pub artifact: String,
    pub summary: String,
    pub workaround: String,
}

impl ConsumerFinding {
    pub fn new(
        artifact: impl Into<String>,
        summary: impl Into<String>,
        workaround: impl Into<String>,
    ) -> Self {
        Self {
            artifact: artifact.into(),
            summary: summary.into(),
            workaround: workaround.into(),
        }
    }
}

/// The workarounds this harness carries to consume the published TypeScript
/// artifacts from Node.
///
/// Recorded on every run that includes a TypeScript leg, because they are
/// properties of the artifact rather than of the run: a consumer installing the
/// same package needs both. Emitting an empty list while carrying them is how a
/// pass comes to speak for something a consumer cannot actually do.
#[must_use]
pub fn typescript_consumer_findings() -> Vec<ConsumerFinding> {
    vec![
        ConsumerFinding::new(
            "@miden-sdk/miden-sdk",
            "the native Node entry omits FeltArray, NoteAndArgsArray and NoteArray, which \
             @openzeppelin/miden-multisig-client imports, so a plain import from Node fails on \
             `FeltArray is not a constructor`",
            "the suite aliases the package to its browser WASM build (dist/st/index.js)",
        ),
        ConsumerFinding::new(
            "@miden-sdk/miden-sdk",
            "the Miden RPC and prover endpoints sit behind a balancer whose gRPC target group \
             accepts HTTP/2 only, while Node's built-in fetch is HTTP/1.1; the balancer answers \
             HTTP 464 with no headers, which the gRPC-web client reports as `missing \
             content-type header in gRPC response`",
            "tests/qualification/h2Fetch.ts routes gRPC-web calls over node:http2",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The findings exist to qualify what a pass means, so an empty list is the
    /// failure mode: it reads as "no workarounds needed".
    #[test]
    fn the_typescript_findings_are_not_empty() {
        let findings = typescript_consumer_findings();
        assert!(!findings.is_empty());
        for finding in &findings {
            assert!(!finding.artifact.is_empty());
            assert!(!finding.summary.is_empty());
            assert!(!finding.workaround.is_empty());
        }
    }
}
