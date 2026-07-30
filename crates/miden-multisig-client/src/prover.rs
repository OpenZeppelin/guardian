use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use miden_client::RemoteTransactionProver;
use miden_client::transaction::TransactionProver;
use miden_protocol::transaction::{ProvenTransaction, TransactionInputs};
use miden_tx::TransactionProverError;
use tonic::Code;
use url::Url;

use crate::error::{MultisigError, Result};

const DEFAULT_MAX_ATTEMPTS: u32 = 2;
const BASE_DELAY_MS: u64 = 500;
const MAX_DELAY_MS: u64 = 8_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProverRetryPolicy {
    max_attempts: u32,
}

impl Default for ProverRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }
}

impl ProverRetryPolicy {
    #[must_use]
    pub fn new(max_attempts: u32) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
        }
    }

    #[must_use]
    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProverConfig {
    url: Option<Url>,
    retry_policy: ProverRetryPolicy,
}

impl ProverConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_url(mut self, url: impl AsRef<str>) -> Result<Self> {
        self.url = Some(parse_prover_url(url.as_ref())?);
        Ok(self)
    }

    #[must_use]
    pub fn with_retry_policy(mut self, retry_policy: ProverRetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    #[must_use]
    pub fn url(&self) -> Option<&str> {
        self.url.as_ref().map(Url::as_str)
    }

    #[must_use]
    pub fn retry_policy(&self) -> &ProverRetryPolicy {
        &self.retry_policy
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProverSelection {
    Local,
    Remote {
        endpoint: String,
        custom: bool,
        retry_policy: ProverRetryPolicy,
    },
}

impl ProverConfig {
    pub(crate) fn resolve(&self, default_remote_endpoint: Option<&str>) -> ProverSelection {
        if let Some(url) = &self.url {
            return ProverSelection::Remote {
                endpoint: url.as_str().to_owned(),
                custom: true,
                retry_policy: self.retry_policy.clone(),
            };
        }

        match default_remote_endpoint {
            Some(endpoint) => ProverSelection::Remote {
                endpoint: endpoint.to_owned(),
                custom: false,
                retry_policy: self.retry_policy.clone(),
            },
            None => ProverSelection::Local,
        }
    }
}

fn parse_prover_url(value: &str) -> Result<Url> {
    let trimmed = value.trim();
    let url =
        Url::parse(trimmed).map_err(|error| MultisigError::InvalidProverUrl(error.to_string()))?;

    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(MultisigError::InvalidProverUrl(
            "must be an absolute HTTP(S) URL with a host".to_string(),
        ));
    }

    Ok(url)
}

#[async_trait::async_trait]
trait RetryRuntime: Send + Sync {
    async fn sleep(&self, duration: Duration);
    fn unit_random(&self) -> f64;
}

struct ProductionRetryRuntime;

#[async_trait::async_trait]
impl RetryRuntime for ProductionRetryRuntime {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }

    fn unit_random(&self) -> f64 {
        rand::random()
    }
}

pub(crate) struct RetryingTransactionProver {
    inner: Arc<dyn TransactionProver + Send + Sync>,
    policy: ProverRetryPolicy,
    runtime: Arc<dyn RetryRuntime>,
}

impl RetryingTransactionProver {
    pub(crate) fn remote(endpoint: impl Into<String>, policy: ProverRetryPolicy) -> Self {
        Self {
            inner: Arc::new(RemoteTransactionProver::new(endpoint)),
            policy,
            runtime: Arc::new(ProductionRetryRuntime),
        }
    }

    #[cfg(test)]
    fn with_runtime(
        inner: Arc<dyn TransactionProver + Send + Sync>,
        policy: ProverRetryPolicy,
        runtime: Arc<dyn RetryRuntime>,
    ) -> Self {
        Self {
            inner,
            policy,
            runtime,
        }
    }
}

#[async_trait::async_trait]
impl TransactionProver for RetryingTransactionProver {
    async fn prove(
        &self,
        tx_inputs: TransactionInputs,
    ) -> std::result::Result<ProvenTransaction, TransactionProverError> {
        for attempt in 0..self.policy.max_attempts {
            match self.inner.prove(tx_inputs.clone()).await {
                Ok(proven) => return Ok(proven),
                Err(error)
                    if is_transient_prover_error(&error)
                        && attempt + 1 < self.policy.max_attempts =>
                {
                    let delay = retry_delay(attempt, self.runtime.unit_random());
                    self.runtime.sleep(delay).await;
                }
                Err(error) => return Err(error),
            }
        }

        unreachable!("a normalized retry policy always performs at least one attempt")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StructuredEvidence {
    Transient,
    Permanent,
    Indeterminate,
}

fn grpc_evidence(code: Code) -> StructuredEvidence {
    match code {
        Code::Cancelled | Code::DeadlineExceeded | Code::Unavailable | Code::ResourceExhausted => {
            StructuredEvidence::Transient
        }
        Code::InvalidArgument
        | Code::FailedPrecondition
        | Code::PermissionDenied
        | Code::Unauthenticated
        | Code::NotFound
        | Code::AlreadyExists
        | Code::OutOfRange
        | Code::Unimplemented
        | Code::Aborted
        | Code::Internal
        | Code::DataLoss => StructuredEvidence::Permanent,
        Code::Unknown | Code::Ok => StructuredEvidence::Indeterminate,
    }
}

fn http_evidence(message: &str) -> Option<StructuredEvidence> {
    const TRANSIENT: [u16; 5] = [408, 429, 502, 503, 504];
    let mut found_transient = false;
    let mut found_permanent = false;

    for status in 400..=599 {
        let structured = [
            format!("http {status}"),
            format!("http status {status}"),
            format!("status: {status}"),
            format!("status {status}"),
        ];
        if structured.iter().any(|pattern| message.contains(pattern)) {
            if TRANSIENT.contains(&status) {
                found_transient = true;
            } else {
                found_permanent = true;
            }
        }
    }

    if found_permanent {
        Some(StructuredEvidence::Permanent)
    } else if found_transient {
        Some(StructuredEvidence::Transient)
    } else {
        None
    }
}

fn flattened_grpc_evidence(message: &str) -> Option<StructuredEvidence> {
    let normalized = message
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .collect::<String>();
    let patterns = [
        ("grpccodecancelled", Code::Cancelled),
        ("grpccodecanceled", Code::Cancelled),
        ("grpccodedeadlineexceeded", Code::DeadlineExceeded),
        ("grpccodeunavailable", Code::Unavailable),
        ("grpccoderesourceexhausted", Code::ResourceExhausted),
        ("grpccodeinvalidargument", Code::InvalidArgument),
        ("grpccodefailedprecondition", Code::FailedPrecondition),
        ("grpccodepermissiondenied", Code::PermissionDenied),
        ("grpccodeunauthenticated", Code::Unauthenticated),
        ("grpccodenotfound", Code::NotFound),
        ("grpccodealreadyexists", Code::AlreadyExists),
        ("grpccodeoutofrange", Code::OutOfRange),
        ("grpccodeunimplemented", Code::Unimplemented),
        ("grpccodeaborted", Code::Aborted),
        ("grpccodeinternal", Code::Internal),
        ("grpccodedataloss", Code::DataLoss),
        ("grpccodeunknown", Code::Unknown),
    ];

    let mut found_transient = false;
    let mut found_permanent = false;

    for (pattern, code) in patterns {
        if normalized.contains(pattern) {
            match grpc_evidence(code) {
                StructuredEvidence::Transient => found_transient = true,
                StructuredEvidence::Permanent => found_permanent = true,
                StructuredEvidence::Indeterminate => {}
            }
        }
    }

    if found_permanent {
        Some(StructuredEvidence::Permanent)
    } else if found_transient {
        Some(StructuredEvidence::Transient)
    } else {
        None
    }
}

fn flattened_transient(message: &str) -> bool {
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

pub(crate) fn is_transient_prover_error(error: &TransactionProverError) -> bool {
    let mut messages = Vec::new();
    let mut has_transient = false;
    let mut has_permanent = false;
    let mut current: Option<&(dyn Error + 'static)> = Some(error);

    while let Some(cause) = current {
        let message = cause.to_string().to_ascii_lowercase();
        if let Some(status) = cause.downcast_ref::<tonic::Status>() {
            match grpc_evidence(status.code()) {
                StructuredEvidence::Transient => has_transient = true,
                StructuredEvidence::Permanent => has_permanent = true,
                StructuredEvidence::Indeterminate => {}
            }
        }
        if let Some(evidence) = http_evidence(&message) {
            match evidence {
                StructuredEvidence::Transient => has_transient = true,
                StructuredEvidence::Permanent => has_permanent = true,
                StructuredEvidence::Indeterminate => {}
            }
        }
        if let Some(evidence) = flattened_grpc_evidence(&message) {
            match evidence {
                StructuredEvidence::Transient => has_transient = true,
                StructuredEvidence::Permanent => has_permanent = true,
                StructuredEvidence::Indeterminate => {}
            }
        }
        messages.push(message);
        current = cause.source();
    }

    if has_permanent {
        return false;
    }
    if has_transient {
        return true;
    }

    messages.iter().any(|message| flattened_transient(message))
}

fn retry_delay(retry_index: u32, unit_random: f64) -> Duration {
    let exponent = retry_index.min(127);
    let raw = u128::from(BASE_DELAY_MS).saturating_mul(1_u128 << exponent);
    let bounded_random = unit_random.clamp(0.0, 1.0 - f64::EPSILON);
    let factor = 0.75 + bounded_random * 0.5;
    let jittered = (raw as f64 * factor).floor();
    Duration::from_millis((jittered as u64).min(MAX_DELAY_MS))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fmt;
    use std::sync::Mutex;

    use miden_client::testing::{Auth, MockChain};
    use miden_protocol::account::AccountBuilder;
    use miden_standards::account::wallets::BasicWallet;
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Fixtures {
        attempt_budgets: Vec<AttemptBudget>,
        endpoints: Vec<EndpointFixture>,
        classifications: Vec<ClassificationFixture>,
        delays: Vec<DelayFixture>,
    }

    #[derive(Deserialize)]
    struct AttemptBudget {
        input: Option<u32>,
        normalized: u32,
    }

    #[derive(Deserialize)]
    struct EndpointFixture {
        input: String,
        valid: bool,
        canonical: Option<String>,
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
            "../../../speckit/features/001-prover-retry-policy/contracts/prover-policy-fixtures.json"
        ))
        .expect("fixtures must parse")
    }

    fn fixture_error(chain: &[ErrorFixture]) -> TransactionProverError {
        let nested = chain.iter().rev().fold(None, |source, item| {
            let message = match (&item.code, item.status) {
                (Some(code), _) => format!("grpc code: {code}; {}", item.message),
                (_, Some(status)) => format!("http status {status}; {}", item.message),
                _ => item.message.clone(),
            };
            Some(Box::new(FixtureError { message, source }))
        });
        TransactionProverError::other_with_source(
            "fixture proving failure",
            *nested.expect("classification chains are non-empty"),
        )
    }

    #[test]
    fn attempt_budget_vectors_match_contract() {
        for fixture in fixtures().attempt_budgets {
            let policy = fixture
                .input
                .map(ProverRetryPolicy::new)
                .unwrap_or_default();
            assert_eq!(policy.max_attempts(), fixture.normalized);
        }
    }

    #[test]
    fn endpoint_vectors_match_contract() {
        for fixture in fixtures().endpoints {
            let result = ProverConfig::new().with_url(&fixture.input);
            assert_eq!(result.is_ok(), fixture.valid, "input: {:?}", fixture.input);
            if let Some(canonical) = fixture.canonical {
                assert_eq!(result.unwrap().url(), Some(canonical.as_str()));
            }
        }
    }

    #[test]
    fn classification_vectors_match_contract() {
        for fixture in fixtures().classifications {
            let error = fixture_error(&fixture.chain);
            assert_eq!(
                is_transient_prover_error(&error),
                fixture.transient,
                "fixture: {}",
                fixture.name
            );
        }
    }

    #[test]
    fn typed_tonic_statuses_follow_whole_chain_precedence() {
        let unavailable = TransactionProverError::other_with_source(
            "failed to prove transaction",
            tonic::Status::unavailable("temporarily unavailable"),
        );
        assert!(is_transient_prover_error(&unavailable));

        let invalid = TransactionProverError::other_with_source(
            "timeout while proving",
            tonic::Status::invalid_argument("invalid proof"),
        );
        assert!(!is_transient_prover_error(&invalid));

        let mixed_http =
            TransactionProverError::other("upstream http status 408 followed by http status 400");
        assert!(!is_transient_prover_error(&mixed_http));

        let flattened_not_found =
            TransactionProverError::other("failed to prove transaction: grpc code: NotFound");
        assert!(!is_transient_prover_error(&flattened_not_found));
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
    fn custom_selection_overrides_local_and_default_remote() {
        let custom = ProverConfig::new()
            .with_url("https://prover.example")
            .unwrap();
        for default in [None, Some("https://tx-prover.testnet.miden.io")] {
            assert!(matches!(
                custom.resolve(default),
                ProverSelection::Remote { custom: true, .. }
            ));
        }
        assert_eq!(ProverConfig::new().resolve(None), ProverSelection::Local);
    }

    #[derive(Default)]
    struct RecordingRuntime {
        sleeps: Mutex<Vec<Duration>>,
    }

    #[async_trait::async_trait]
    impl RetryRuntime for RecordingRuntime {
        async fn sleep(&self, duration: Duration) {
            self.sleeps.lock().unwrap().push(duration);
        }

        fn unit_random(&self) -> f64 {
            0.5
        }
    }

    struct FailingProver {
        errors: Mutex<VecDeque<TransactionProverError>>,
        inputs: Mutex<Vec<TransactionInputs>>,
    }

    #[async_trait::async_trait]
    impl TransactionProver for FailingProver {
        async fn prove(
            &self,
            inputs: TransactionInputs,
        ) -> std::result::Result<ProvenTransaction, TransactionProverError> {
            self.inputs.lock().unwrap().push(inputs);
            Err(self
                .errors
                .lock()
                .unwrap()
                .pop_front()
                .expect("one fixture error per expected attempt"))
        }
    }

    fn transaction_inputs() -> TransactionInputs {
        let mut chain = MockChain::new();
        chain.prove_next_block().unwrap();
        let account = AccountBuilder::new([0; 32])
            .with_auth_component(Auth::IncrNonce)
            .with_component(BasicWallet)
            .build()
            .unwrap();
        chain.get_transaction_inputs(&account, &[], &[]).unwrap()
    }

    #[tokio::test]
    async fn retries_the_same_inputs_and_returns_the_final_upstream_error() {
        let inner = Arc::new(FailingProver {
            errors: Mutex::new(VecDeque::from([
                TransactionProverError::other("service unavailable"),
                TransactionProverError::other("deadline exceeded: final"),
            ])),
            inputs: Mutex::new(Vec::new()),
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let prover = RetryingTransactionProver::with_runtime(
            inner.clone(),
            ProverRetryPolicy::new(2),
            runtime.clone(),
        );
        let inputs = transaction_inputs();

        let error = prover.prove(inputs.clone()).await.unwrap_err();

        assert_eq!(error.to_string(), "deadline exceeded: final");
        let recorded_inputs = inner.inputs.lock().unwrap();
        assert_eq!(recorded_inputs.len(), 2);
        assert_eq!(recorded_inputs[0], inputs);
        assert_eq!(recorded_inputs[1], inputs);
        assert_eq!(
            runtime.sleeps.lock().unwrap().as_slice(),
            [Duration::from_millis(500)]
        );
    }

    #[tokio::test]
    async fn permanent_failure_does_not_retry_or_sleep() {
        let inner = Arc::new(FailingProver {
            errors: Mutex::new(VecDeque::from([TransactionProverError::other(
                "transaction kernel assertion failed",
            )])),
            inputs: Mutex::new(Vec::new()),
        });
        let runtime = Arc::new(RecordingRuntime::default());
        let prover = RetryingTransactionProver::with_runtime(
            inner.clone(),
            ProverRetryPolicy::new(5),
            runtime.clone(),
        );

        prover.prove(transaction_inputs()).await.unwrap_err();

        assert_eq!(inner.inputs.lock().unwrap().len(), 1);
        assert!(runtime.sleeps.lock().unwrap().is_empty());
    }
}
