use std::error::Error;
use std::future::Future;
use std::time::Duration;

pub const BASE_DELAY_MS: u64 = 500;
pub const MAX_DELAY_MS: u64 = 8_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    max_attempts: u32,
}

impl RetryPolicy {
    #[must_use]
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
        }
    }

    #[must_use]
    pub fn single_attempt() -> Self {
        Self::new(1)
    }

    #[must_use]
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    #[must_use]
    pub fn retries_enabled(&self) -> bool {
        self.max_attempts > 1
    }
}

/// How a read selects its attempt budget. Callers holding a lease or other
/// structural retry (e.g. a canonicalization pass) must pin reads to a
/// single attempt; the choice is deliberate at every read site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RpcReadMode {
    Configured,
    SingleAttempt,
}

#[async_trait::async_trait]
pub trait RetryRuntime: Send + Sync {
    async fn sleep(&self, duration: Duration);
    fn unit_random(&self) -> f64;
}

pub struct ProductionRetryRuntime;

#[async_trait::async_trait]
impl RetryRuntime for ProductionRetryRuntime {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    fn unit_random(&self) -> f64 {
        rand::random()
    }
}

/// Runs `op` under an attempt budget: the budget is consulted before the
/// error is classified, transient failures back off with [`retry_delay`],
/// and permanent failures or the final attempt return the error unchanged.
/// `on_retry` fires once per retry, before the backoff sleep.
pub async fn run_retries<T, E, F, Fut>(
    max_attempts: u32,
    runtime: &dyn RetryRuntime,
    is_transient: impl Fn(&E) -> bool,
    on_retry: impl Fn(u32, &E),
    op: F,
) -> Result<T, E>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let attempts = max_attempts.max(1);
    for attempt in 0..attempts {
        match op().await {
            Ok(value) => return Ok(value),
            Err(error) if attempt + 1 < attempts && is_transient(&error) => {
                on_retry(attempt, &error);
                runtime
                    .sleep(retry_delay(attempt, runtime.unit_random()))
                    .await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the attempt budget always admits at least one attempt")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StructuredEvidence {
    Transient,
    Permanent,
    Indeterminate,
}

/// Accumulates evidence under the one precedence rule of the classifier:
/// permanent anywhere vetoes transient anywhere.
#[derive(Default)]
struct EvidenceLedger {
    transient: bool,
    permanent: bool,
}

impl EvidenceLedger {
    fn note(&mut self, evidence: StructuredEvidence) {
        match evidence {
            StructuredEvidence::Transient => self.transient = true,
            StructuredEvidence::Permanent => self.permanent = true,
            StructuredEvidence::Indeterminate => {}
        }
    }

    fn verdict(&self) -> Option<StructuredEvidence> {
        if self.permanent {
            Some(StructuredEvidence::Permanent)
        } else if self.transient {
            Some(StructuredEvidence::Transient)
        } else {
            None
        }
    }
}

/// Classifies a numeric gRPC status code. Permanent codes veto retries even
/// when transient wording appears elsewhere in the error chain.
#[must_use]
pub fn grpc_code_evidence(code: i32) -> StructuredEvidence {
    match code {
        1 | 4 | 8 | 14 => StructuredEvidence::Transient,
        3 | 5 | 6 | 7 | 9 | 10 | 11 | 12 | 13 | 15 | 16 => StructuredEvidence::Permanent,
        _ => StructuredEvidence::Indeterminate,
    }
}

/// Extracts HTTP statuses by scanning for the marker wordings and parsing
/// the number that follows. Mirrors the TypeScript classifier's pattern —
/// `http`, `http status`, or `status` on a word boundary, an optional `code`
/// token, an optional colon, then exactly three digits ending on a word
/// boundary — and is pinned to it by the classification fixtures.
#[must_use]
pub fn http_evidence(message: &str) -> Option<StructuredEvidence> {
    const TRANSIENT: [u16; 5] = [408, 429, 502, 503, 504];
    const STATUS_MARKERS: [&str; 3] = ["http status", "http", "status"];

    let bytes = message.as_bytes();
    let mut ledger = EvidenceLedger::default();
    for marker in STATUS_MARKERS {
        for (start, matched) in message.match_indices(marker) {
            let on_word_boundary = start
                .checked_sub(1)
                .and_then(|index| bytes.get(index))
                .is_none_or(|byte| !byte.is_ascii_alphanumeric() && *byte != b'_');
            if !on_word_boundary {
                continue;
            }
            let Some(status) = leading_status(bytes, start + matched.len()) else {
                continue;
            };
            if !(400..=599).contains(&status) {
                continue;
            }
            ledger.note(if TRANSIENT.contains(&status) {
                StructuredEvidence::Transient
            } else {
                StructuredEvidence::Permanent
            });
        }
    }
    ledger.verdict()
}

fn skip_ascii_whitespace(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    cursor
}

fn leading_status(bytes: &[u8], from: usize) -> Option<u16> {
    let mut cursor = skip_ascii_whitespace(bytes, from);
    if cursor > from && bytes[cursor..].starts_with(b"code") {
        cursor = skip_ascii_whitespace(bytes, cursor + 4);
    }
    if bytes.get(cursor) == Some(&b':') {
        cursor = skip_ascii_whitespace(bytes, cursor + 1);
    }
    let mut end = cursor;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end - cursor != 3 {
        return None;
    }
    if bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
    {
        return None;
    }
    std::str::from_utf8(&bytes[cursor..end]).ok()?.parse().ok()
}

#[must_use]
pub fn flattened_grpc_evidence(message: &str) -> Option<StructuredEvidence> {
    let normalized = message
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    let patterns = [
        ("grpccodecancelled", 1),
        ("grpccodecanceled", 1),
        ("grpccodedeadlineexceeded", 4),
        ("grpccodeunavailable", 14),
        ("grpccoderesourceexhausted", 8),
        ("grpccodeinvalidargument", 3),
        ("grpccodefailedprecondition", 9),
        ("grpccodepermissiondenied", 7),
        ("grpccodeunauthenticated", 16),
        ("grpccodenotfound", 5),
        ("grpccodealreadyexists", 6),
        ("grpccodeoutofrange", 11),
        ("grpccodeunimplemented", 12),
        ("grpccodeaborted", 10),
        ("grpccodeinternal", 13),
        ("grpccodedataloss", 15),
        ("grpccodeunknown", 2),
    ];

    let mut ledger = EvidenceLedger::default();
    for (pattern, code) in patterns {
        if normalized.contains(pattern) {
            ledger.note(grpc_code_evidence(code));
        }
    }
    ledger.verdict()
}

/// Last-resort transient wording, consulted only when the whole chain carried
/// no status-level evidence. Guarded by the negative classification fixtures
/// (server rejections must never match) — extend with care.
#[must_use]
pub fn flattened_transient(message: &str) -> bool {
    [
        "cancelled",
        "canceled",
        "deadline exceeded",
        "timeout",
        "unavailable",
        "resource exhausted",
        "request timeout",
        "too many requests",
        "rate limited",
        "rate limit",
        "bad gateway",
        "service unavailable",
        "gateway timeout",
        "i/o timeout",
        "io timeout",
        "connection reset",
        "broken pipe",
    ]
    .iter()
    .any(|signal| message.contains(signal))
}

/// Connection-failure wording the retry window cannot fix: TLS and
/// certificate problems, or an endpoint that never parsed. Everything else
/// (refused, reset, timeout, unresolved name) is the peer-still-booting
/// case and stays retryable.
pub const CONNECT_PERMANENT_SIGNALS: [&str; 4] =
    ["certificate", "tls", "invalid uri", "unsupported scheme"];

/// Walks a connection error's chain and reports whether it carries wording
/// from [`CONNECT_PERMANENT_SIGNALS`].
pub fn connect_failure_is_permanent(error: &(dyn Error + 'static)) -> bool {
    let mut message = error.to_string().to_ascii_lowercase();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push(' ');
        message.push_str(&cause.to_string().to_ascii_lowercase());
        source = cause.source();
    }
    CONNECT_PERMANENT_SIGNALS
        .iter()
        .any(|signal| message.contains(signal))
}

/// Node-RPC transient extension over [`flattened_transient`]: the node's
/// transport layer renders dropped connections with wording the prover policy
/// deliberately rejects (a bare "connection error" from a prover is treated as
/// its considered answer). Guarded by the negative classification fixtures.
pub const RPC_TRANSPORT_SIGNALS: [&str; 2] = ["connection error", "transport error"];

/// Walks the whole error chain accumulating evidence: permanent anywhere
/// vetoes transient anywhere; the transport-text fallback fires only when no
/// status-level evidence exists in any link. `link_evidence` supplies
/// transport-specific typed classification (e.g. a `tonic::Status` downcast).
pub fn is_transient_error<F>(error: &(dyn Error + 'static), link_evidence: F) -> bool
where
    F: Fn(&(dyn Error + 'static)) -> StructuredEvidence,
{
    is_transient_error_with(error, link_evidence, &[])
}

/// [`is_transient_error`] with domain-specific additions to the transient
/// text fallback. Extras participate under the same permanent-wins rule.
pub fn is_transient_error_with<F>(
    error: &(dyn Error + 'static),
    link_evidence: F,
    extra_transient_signals: &[&str],
) -> bool
where
    F: Fn(&(dyn Error + 'static)) -> StructuredEvidence,
{
    let mut has_transient = false;
    let mut has_fallback = false;
    let mut current: Option<&(dyn Error + 'static)> = Some(error);

    while let Some(cause) = current {
        let message = cause.to_string().to_ascii_lowercase();
        let mut ledger = EvidenceLedger::default();
        ledger.note(link_evidence(cause));
        if let Some(evidence) = http_evidence(&message) {
            ledger.note(evidence);
        }
        if let Some(evidence) = flattened_grpc_evidence(&message) {
            ledger.note(evidence);
        }
        match ledger.verdict() {
            Some(StructuredEvidence::Permanent) => return false,
            Some(_) => has_transient = true,
            None => {}
        }
        has_fallback = has_fallback
            || flattened_transient(&message)
            || extra_transient_signals
                .iter()
                .any(|signal| message.contains(signal));
        current = cause.source();
    }

    has_transient || has_fallback
}

#[must_use]
pub fn retry_delay(retry_index: u32, unit_random: f64) -> Duration {
    let exponent = retry_index.min(127);
    let raw = u128::from(BASE_DELAY_MS).saturating_mul(1_u128 << exponent);
    let bounded_random = unit_random.clamp(0.0, 1.0 - f64::EPSILON);
    let factor = 0.75 + bounded_random * 0.5;
    let jittered = (raw as f64 * factor).floor();
    Duration::from_millis((jittered as u64).min(MAX_DELAY_MS))
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixtures {
        classifications: Vec<ClassificationFixture>,
        delays: Vec<DelayFixture>,
    }

    #[derive(Deserialize)]
    struct ClassificationFixture {
        name: String,
        chain: Vec<ErrorFixture>,
        transient: bool,
    }

    #[derive(Deserialize)]
    struct ErrorFixture {
        code: Option<String>,
        status: Option<u16>,
        message: String,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DelayFixture {
        retry_index: u32,
        unit_random: f64,
        delay_ms: u64,
    }

    #[derive(Debug)]
    struct FixtureError {
        message: String,
        source: Option<Box<FixtureError>>,
    }

    impl fmt::Display for FixtureError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.message)
        }
    }

    impl Error for FixtureError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source.as_deref().map(|source| source as _)
        }
    }

    fn fixtures() -> Fixtures {
        serde_json::from_str(include_str!(
            "../../../fixtures/miden-multisig-client/rpc-policy-fixtures.json"
        ))
        .expect("fixtures must parse")
    }

    fn fixture_error(chain: &[ErrorFixture]) -> FixtureError {
        let nested = chain.iter().rev().fold(None, |source, item| {
            let message = match (&item.code, item.status) {
                (Some(code), _) => format!("grpc code: {code}; {}", item.message),
                (_, Some(status)) => format!("http status {status}; {}", item.message),
                _ => item.message.clone(),
            };
            Some(Box::new(FixtureError { message, source }))
        });
        *nested.expect("classification chains are non-empty")
    }

    #[test]
    fn classification_vectors_match_contract() {
        for fixture in fixtures().classifications {
            let error = fixture_error(&fixture.chain);
            assert_eq!(
                is_transient_error_with(
                    &error,
                    |_| StructuredEvidence::Indeterminate,
                    &RPC_TRANSPORT_SIGNALS,
                ),
                fixture.transient,
                "fixture: {}",
                fixture.name
            );
        }
    }

    #[test]
    fn transport_extension_is_opt_in() {
        let error = FixtureError {
            message: "transport error while proving".to_string(),
            source: None,
        };
        assert!(!is_transient_error(&error, |_| {
            StructuredEvidence::Indeterminate
        }));
        assert!(is_transient_error_with(
            &error,
            |_| StructuredEvidence::Indeterminate,
            &RPC_TRANSPORT_SIGNALS,
        ));
    }

    #[test]
    fn delay_vectors_match_contract() {
        for fixture in fixtures().delays {
            assert_eq!(
                retry_delay(fixture.retry_index, fixture.unit_random).as_millis(),
                u128::from(fixture.delay_ms)
            );
        }
    }

    #[test]
    fn typed_link_evidence_participates_in_precedence() {
        let outer = FixtureError {
            message: "temporarily unavailable".to_string(),
            source: Some(Box::new(FixtureError {
                message: "permission denied".to_string(),
                source: None,
            })),
        };
        let by_message = |cause: &(dyn Error + 'static)| {
            let message = cause.to_string();
            if message.contains("unavailable") {
                StructuredEvidence::Transient
            } else if message.contains("permission denied") {
                StructuredEvidence::Permanent
            } else {
                StructuredEvidence::Indeterminate
            }
        };
        assert!(!is_transient_error(&outer, by_message));

        let transient_only = FixtureError {
            message: "temporarily unavailable".to_string(),
            source: None,
        };
        assert!(is_transient_error(&transient_only, by_message));
    }

    #[test]
    fn grpc_code_partition_matches_tonic_codes() {
        assert_eq!(grpc_code_evidence(1), StructuredEvidence::Transient);
        assert_eq!(grpc_code_evidence(4), StructuredEvidence::Transient);
        assert_eq!(grpc_code_evidence(8), StructuredEvidence::Transient);
        assert_eq!(grpc_code_evidence(14), StructuredEvidence::Transient);
        assert_eq!(grpc_code_evidence(13), StructuredEvidence::Permanent);
        assert_eq!(grpc_code_evidence(10), StructuredEvidence::Permanent);
        assert_eq!(grpc_code_evidence(0), StructuredEvidence::Indeterminate);
        assert_eq!(grpc_code_evidence(2), StructuredEvidence::Indeterminate);
        assert_eq!(grpc_code_evidence(99), StructuredEvidence::Indeterminate);
    }
}
