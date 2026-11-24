//! Conformance suite for stateless modules.
//!
//! [`Suite`] runs one module in process and through the [`serve`](crate::serve) harness over
//! loopback gRPC, with two instances on one service, and checks that it answers `Describe`
//! for protocol version 1, validates params the same way on both transports, returns the same
//! outcome for every request on both transports, keeps `evaluate` free of side effects
//! (repeated, reordered and concurrent evaluations agree), returns well-formed decisions,
//! answers each request as the suite says it must, and accepts a request at the Guardian's size
//! cap.
//!
//! Consistency alone is not conformance: a module that allows everything is consistent. The
//! suite therefore needs at least one request the module must deny ([`Suite::denies`] or
//! [`Suite::denies_with`]), and checks every stated expectation.
//!
//! ```no_run
//! # async fn example(request: guardian_module_sdk::HookRequest, blocked: guardian_module_sdk::HookRequest) {
//! use guardian_module_sdk::policies::RecipientPolicy;
//! use guardian_module_sdk::conformance::Suite;
//!
//! Suite::new(RecipientPolicy)
//!     .allows("small payment", request)
//!     .denies_with("payment to a blocked recipient", blocked, "recipient_blocked")
//!     .valid_params(serde_json::json!({ "denylist": [] }))
//!     .invalid_params(serde_json::json!({ "unknown": 1 }))
//!     .run()
//!     .await
//!     .assert_conformant();
//! # }
//! ```

use std::fmt;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::server::TcpIncoming;
use tonic::transport::{Channel, Server};

use crate::proto::guardian_module_client::GuardianModuleClient;
use crate::proto::hook_request;
use crate::serve::{serve, status};
use crate::{
    DEFAULT_REQUEST_MAX_BYTES, Decision, DescribeRequest, Effect, Hook, HookRequest, Module,
    PROTOCOL_VERSION, ParamsVerdict, RawTransaction, ValidateParamsRequest, is_valid_reason_code,
};

const INSTANCE_A: &str = "conformance-a";
const INSTANCE_B: &str = "conformance-b";
const TOKEN_A: &str = "conformance-token-a";
const TOKEN_B: &str = "conformance-token-b";
const CLIENT_MESSAGE_MAX_CHARS: usize = 256;
const REPEATS: usize = 3;
const PADDING_ENCODING: &str = "conformance.padding";

type Outcome = Result<Decision, tonic::Code>;

/// What a request must be answered with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Expected {
    Allow,
    Deny,
    DenyWith(String),
    Error,
}

impl Expected {
    fn met_by(&self, outcome: &Outcome) -> bool {
        match (self, outcome) {
            (Expected::Allow, Ok(decision)) => decision.effect == Effect::Allow as i32,
            (Expected::Deny, Ok(decision)) => decision.effect == Effect::Deny as i32,
            (Expected::DenyWith(code), Ok(decision)) => {
                decision.effect == Effect::Deny as i32 && &decision.reason_code == code
            }
            (Expected::Error, Err(code)) => *code == tonic::Code::InvalidArgument,
            (_, _) => false,
        }
    }
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expected::Allow => write!(f, "an allow"),
            Expected::Deny => write!(f, "a deny"),
            Expected::DenyWith(code) => write!(f, "a deny with reason code {code}"),
            Expected::Error => write!(f, "an InvalidRequest error (INVALID_ARGUMENT)"),
        }
    }
}

pub struct Suite<M> {
    module: Arc<M>,
    requests: Vec<(String, HookRequest)>,
    /// One entry per request: what it must be answered with, when the suite says.
    expected: Vec<Option<Expected>>,
    valid_params: Vec<serde_json::Value>,
    invalid_params: Vec<serde_json::Value>,
    request_max_bytes: usize,
}

impl<M: Module + 'static> Suite<M> {
    pub fn new(module: M) -> Self {
        Self {
            module: Arc::new(module),
            requests: Vec::new(),
            expected: Vec::new(),
            valid_params: Vec::new(),
            invalid_params: Vec::new(),
            request_max_bytes: DEFAULT_REQUEST_MAX_BYTES,
        }
    }

    /// A request the module must answer the same way on every transport and every time,
    /// whatever the answer is. The suite sets `instance_id`; everything else is sent as given.
    pub fn request(self, name: impl Into<String>, request: HookRequest) -> Self {
        self.expecting(name, request, None)
    }

    /// A request the module must allow.
    pub fn allows(self, name: impl Into<String>, request: HookRequest) -> Self {
        self.expecting(name, request, Some(Expected::Allow))
    }

    /// A request the module must deny. Every suite needs one: a module that refuses nothing
    /// answers consistently too.
    pub fn denies(self, name: impl Into<String>, request: HookRequest) -> Self {
        self.expecting(name, request, Some(Expected::Deny))
    }

    /// A request the module must deny with `reason_code`.
    pub fn denies_with(
        self,
        name: impl Into<String>,
        request: HookRequest,
        reason_code: impl Into<String>,
    ) -> Self {
        self.expecting(name, request, Some(Expected::DenyWith(reason_code.into())))
    }

    /// A request the module cannot judge: it must answer `InvalidRequest` (gRPC
    /// `INVALID_ARGUMENT`), which the Guardian refuses whatever the instance's `on_error` says,
    /// never an allow. Any other error would be read as the module's own failure, which a
    /// fail-open instance lets through and which counts toward its circuit breaker.
    pub fn fails(self, name: impl Into<String>, request: HookRequest) -> Self {
        self.expecting(name, request, Some(Expected::Error))
    }

    fn expecting(
        mut self,
        name: impl Into<String>,
        request: HookRequest,
        expected: Option<Expected>,
    ) -> Self {
        self.requests.push((name.into(), request));
        self.expected.push(expected);
        self
    }

    pub fn valid_params(mut self, params: serde_json::Value) -> Self {
        self.valid_params.push(params);
        self
    }

    pub fn invalid_params(mut self, params: serde_json::Value) -> Self {
        self.invalid_params.push(params);
        self
    }

    /// The size of the request sent at the cap; the Guardian's default unless the operator
    /// raises `GUARDIAN_MODULE_REQUEST_MAX_BYTES`.
    pub fn request_max_bytes(mut self, bytes: usize) -> Self {
        self.request_max_bytes = bytes;
        self
    }

    pub async fn run(self) -> Report {
        let mut report = Report::default();
        if self.requests.is_empty() {
            report.fail("setup", "", "the suite needs at least one request");
        }
        if !self
            .expected
            .iter()
            .any(|expected| matches!(expected, Some(Expected::Deny | Expected::DenyWith(_))))
        {
            report.fail(
                "setup",
                "",
                "the suite needs at least one request the module must deny (`denies` or `denies_with`): a module that refuses nothing would conform otherwise",
            );
        }
        if self.valid_params.is_empty() {
            report.fail(
                "setup",
                "",
                "the suite needs at least one valid params value",
            );
        }
        let (channel, shutdown) = match self.start_remote().await {
            Ok(started) => started,
            Err(detail) => {
                report.fail("setup", "", detail);
                return report;
            }
        };
        let remote = Remote {
            client: GuardianModuleClient::new(channel),
        };
        self.check_describe(&remote, &mut report).await;
        self.check_params(&remote, &mut report).await;
        let baseline = self.check_transport(&remote, &mut report).await;
        self.check_decisions(&baseline, &mut report);
        self.check_expectations(&baseline, &mut report);
        self.check_purity(&baseline, &mut report).await;
        self.check_instances(&remote, &baseline, &mut report).await;
        self.check_size_cap(&remote, &mut report).await;
        let _ = shutdown.send(());
        report
    }

    async fn start_remote(&self) -> Result<(Channel, oneshot::Sender<()>), String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("bind loopback: {e}"))?;
        let addr = listener
            .local_addr()
            .map_err(|e| format!("loopback address: {e}"))?;
        let service = serve(Arc::clone(&self.module))
            .instance(INSTANCE_A, TOKEN_A)
            .instance(INSTANCE_B, TOKEN_B)
            .into_service();
        let (shutdown, stopped) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = Server::builder()
                .add_service(service)
                .serve_with_incoming_shutdown(TcpIncoming::from(listener), async {
                    let _ = stopped.await;
                })
                .await;
        });
        let channel = Channel::from_shared(format!("http://{addr}"))
            .map_err(|e| e.to_string())?
            .connect()
            .await
            .map_err(|e| format!("connect to the served module: {e}"))?;
        Ok((channel, shutdown))
    }

    async fn check_describe(&self, remote: &Remote, report: &mut Report) {
        let guardian = DescribeRequest {
            guardian_protocol_version: PROTOCOL_VERSION,
            guardian_min_protocol_version: PROTOCOL_VERSION,
            guardian_capabilities: Vec::new(),
        };
        let local = match self.module.describe(&guardian).await {
            Ok(info) => info,
            Err(error) => {
                report.fail("describe", "", format!("in process: {error}"));
                return;
            }
        };
        if local.protocol_version != PROTOCOL_VERSION {
            report.fail(
                "describe",
                "",
                format!(
                    "protocol_version {} is not {PROTOCOL_VERSION}",
                    local.protocol_version
                ),
            );
        }
        if local.name.is_empty() {
            report.fail("describe", "", "name is empty");
        }
        if local.hooks.is_empty() {
            report.fail("describe", "", "declares no hooks");
        }
        for hook in &local.hooks {
            if !matches!(Hook::try_from(*hook), Ok(known) if known != Hook::Unspecified) {
                report.fail("describe", "", format!("declares unknown hook {hook}"));
            }
        }
        match remote.describe(guardian).await {
            Ok(served) if served == local => {}
            Ok(served) => report.fail(
                "describe",
                "",
                format!("over gRPC {served:?}, in process {local:?}"),
            ),
            Err(code) => report.fail("describe", "", format!("over gRPC: {code:?}")),
        }
    }

    async fn check_params(&self, remote: &Remote, report: &mut Report) {
        for params in &self.valid_params {
            let case = params.to_string();
            let Some(verdict) = self.validate_both(remote, params, &case, report).await else {
                continue;
            };
            if !verdict.ok {
                report.fail("params", &case, format!("refused: {}", verdict.message));
                continue;
            }
            if verdict.normalized_params.is_empty() {
                continue;
            }
            let normalized =
                match serde_json::from_slice::<serde_json::Value>(&verdict.normalized_params) {
                    Ok(value) if value.is_object() => value,
                    _ => {
                        report.fail("params", &case, "normalized_params is not a JSON object");
                        continue;
                    }
                };
            match self.module.validate_params(&json_bytes(&normalized)).await {
                Ok(again) if again.ok && normalizes_to(&again, &normalized) => {}
                Ok(again) => report.fail(
                    "params",
                    &case,
                    format!("normalized params do not validate to themselves: {again:?}"),
                ),
                Err(error) => report.fail("params", &case, format!("normalized params: {error}")),
            }
        }
        for params in &self.invalid_params {
            let case = params.to_string();
            let Some(verdict) = self.validate_both(remote, params, &case, report).await else {
                continue;
            };
            if verdict.ok {
                report.fail("params", &case, "accepted params the suite marks invalid");
            } else if verdict.message.is_empty() {
                report.fail("params", &case, "a refusal carries no message");
            }
        }
    }

    async fn validate_both(
        &self,
        remote: &Remote,
        params: &serde_json::Value,
        case: &str,
        report: &mut Report,
    ) -> Option<ParamsVerdict> {
        let bytes = json_bytes(params);
        let local = match self.module.validate_params(&bytes).await {
            Ok(verdict) => verdict,
            Err(error) => {
                report.fail("params", case, format!("in process: {error}"));
                return None;
            }
        };
        match remote.validate(INSTANCE_A, TOKEN_A, bytes).await {
            Ok(served) if served == local => {}
            Ok(served) => report.fail(
                "params",
                case,
                format!("over gRPC {served:?}, in process {local:?}"),
            ),
            Err(code) => report.fail("params", case, format!("over gRPC: {code:?}")),
        }
        Some(local)
    }

    async fn check_transport(&self, remote: &Remote, report: &mut Report) -> Vec<Outcome> {
        let mut baseline = Vec::with_capacity(self.requests.len());
        for (name, request) in &self.requests {
            let request = for_instance(request, INSTANCE_A);
            let local = evaluate_local(&self.module, &request).await;
            let served = remote.evaluate(TOKEN_A, request).await;
            if served != local {
                report.fail(
                    "transport",
                    name,
                    format!("over gRPC {served:?}, in process {local:?}"),
                );
            }
            baseline.push(local);
        }
        baseline
    }

    fn check_decisions(&self, baseline: &[Outcome], report: &mut Report) {
        for ((name, _), outcome) in self.requests.iter().zip(baseline) {
            let Ok(decision) = outcome else { continue };
            if let Err(detail) = well_formed(decision) {
                report.fail("decision", name, detail);
            }
        }
    }

    fn check_expectations(&self, baseline: &[Outcome], report: &mut Report) {
        for (((name, _), expected), outcome) in
            self.requests.iter().zip(&self.expected).zip(baseline)
        {
            let Some(expected) = expected else { continue };
            if !expected.met_by(outcome) {
                report.fail(
                    "expectation",
                    name,
                    format!("expected {expected}, got {outcome:?}"),
                );
            }
        }
    }

    async fn check_purity(&self, baseline: &[Outcome], report: &mut Report) {
        for ((name, request), expected) in self.requests.iter().zip(baseline) {
            let request = for_instance(request, INSTANCE_A);
            for _ in 0..REPEATS {
                let again = evaluate_local(&self.module, &request).await;
                if &again != expected {
                    report.fail(
                        "purity",
                        name,
                        format!("repeated: {again:?}, first {expected:?}"),
                    );
                    break;
                }
            }
        }
        for ((name, request), expected) in self.requests.iter().zip(baseline).rev() {
            let again = evaluate_local(&self.module, &for_instance(request, INSTANCE_A)).await;
            if &again != expected {
                report.fail(
                    "purity",
                    name,
                    format!("in reverse order: {again:?}, first {expected:?}"),
                );
            }
        }
        let mut concurrent = JoinSet::new();
        for _ in 0..REPEATS {
            for (index, (_, request)) in self.requests.iter().enumerate() {
                let module = Arc::clone(&self.module);
                let request = for_instance(request, INSTANCE_A);
                concurrent.spawn(async move { (index, evaluate_local(&module, &request).await) });
            }
        }
        while let Some(joined) = concurrent.join_next().await {
            match joined {
                Ok((index, outcome)) if outcome != baseline[index] => report.fail(
                    "purity",
                    &self.requests[index].0,
                    format!("concurrently: {outcome:?}, first {:?}", baseline[index]),
                ),
                Ok(_) => {}
                Err(error) => report.fail("purity", "", format!("evaluation panicked: {error}")),
            }
        }
    }

    async fn check_instances(&self, remote: &Remote, baseline: &[Outcome], report: &mut Report) {
        for (name, request) in &self.requests {
            let request = for_instance(request, INSTANCE_B);
            let local = evaluate_local(&self.module, &request).await;
            let served = remote.evaluate(TOKEN_B, request).await;
            if served != local {
                report.fail(
                    "instances",
                    name,
                    format!("second instance over gRPC {served:?}, in process {local:?}"),
                );
            }
        }
        for ((name, request), expected) in self.requests.iter().zip(baseline) {
            let again = remote
                .evaluate(TOKEN_A, for_instance(request, INSTANCE_A))
                .await;
            if &again != expected {
                report.fail(
                    "instances",
                    name,
                    format!("after the second instance ran: {again:?}, first {expected:?}"),
                );
            }
        }
        let Some((name, request)) = self.requests.first() else {
            return;
        };
        let crossed = remote
            .evaluate(TOKEN_A, for_instance(request, INSTANCE_B))
            .await;
        if crossed != Err(tonic::Code::PermissionDenied) {
            report.fail(
                "instances",
                name,
                format!("one instance's token used for another: {crossed:?}"),
            );
        }
        let anonymous = remote
            .evaluate_unauthenticated(for_instance(request, INSTANCE_A))
            .await;
        if anonymous != Err(tonic::Code::Unauthenticated) {
            report.fail("instances", name, format!("no token: {anonymous:?}"));
        }
    }

    async fn check_size_cap(&self, remote: &Remote, report: &mut Report) {
        let Some((name, request)) = self.requests.first() else {
            return;
        };
        let padded = at_size(for_instance(request, INSTANCE_A), self.request_max_bytes);
        let case = format!("{name} at {} bytes", prost::Message::encoded_len(&padded));
        let local = evaluate_local(&self.module, &padded).await;
        let served = remote.evaluate(TOKEN_A, padded).await;
        if served != local {
            report.fail(
                "size cap",
                &case,
                format!("over gRPC {served:?}, in process {local:?}"),
            );
        }
    }
}

struct Remote {
    client: GuardianModuleClient<Channel>,
}

impl Remote {
    async fn describe(&self, guardian: DescribeRequest) -> Result<crate::ModuleInfo, tonic::Code> {
        self.client
            .clone()
            .describe(authorized(guardian, TOKEN_A))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| status.code())
    }

    async fn validate(
        &self,
        instance_id: &str,
        token: &str,
        params: Vec<u8>,
    ) -> Result<ParamsVerdict, tonic::Code> {
        let request = ValidateParamsRequest {
            instance_id: instance_id.into(),
            params,
        };
        self.client
            .clone()
            .validate_params(authorized(request, token))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| status.code())
    }

    async fn evaluate(&self, token: &str, request: HookRequest) -> Outcome {
        self.client
            .clone()
            .evaluate(authorized(request, token))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| status.code())
    }

    async fn evaluate_unauthenticated(&self, request: HookRequest) -> Outcome {
        self.client
            .clone()
            .evaluate(Request::new(request))
            .await
            .map(tonic::Response::into_inner)
            .map_err(|status| status.code())
    }
}

/// What the suite found; empty when the module conforms.
#[derive(Debug, Default)]
pub struct Report {
    failures: Vec<Failure>,
}

impl Report {
    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    pub fn is_conformant(&self) -> bool {
        self.failures.is_empty()
    }

    /// Panics with every failure when the module does not conform.
    pub fn assert_conformant(&self) {
        assert!(self.is_conformant(), "module does not conform:\n{self}");
    }

    fn fail(&mut self, check: &'static str, case: &str, detail: impl Into<String>) {
        self.failures.push(Failure {
            check,
            case: case.to_string(),
            detail: detail.into(),
        });
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for failure in &self.failures {
            writeln!(f, "{failure}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// `setup`, `describe`, `params`, `transport`, `decision`, `purity`, `instances` or
    /// `size cap`.
    pub check: &'static str,
    pub case: String,
    pub detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.case.is_empty() {
            write!(f, "[{}] {}", self.check, self.detail)
        } else {
            write!(f, "[{}] {}: {}", self.check, self.case, self.detail)
        }
    }
}

async fn evaluate_local<M: Module>(module: &M, request: &HookRequest) -> Outcome {
    module
        .evaluate(request)
        .await
        .map_err(|error| status(error).code())
}

fn authorized<T>(message: T, token: &str) -> Request<T> {
    let mut request = Request::new(message);
    if let Ok(value) = MetadataValue::try_from(format!("Bearer {token}")) {
        request.metadata_mut().insert("authorization", value);
    }
    request
}

fn for_instance(request: &HookRequest, instance_id: &str) -> HookRequest {
    HookRequest {
        instance_id: instance_id.into(),
        ..request.clone()
    }
}

fn json_bytes(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

fn normalizes_to(verdict: &ParamsVerdict, normalized: &serde_json::Value) -> bool {
    verdict.normalized_params.is_empty()
        || serde_json::from_slice::<serde_json::Value>(&verdict.normalized_params)
            .is_ok_and(|again| &again == normalized)
}

fn well_formed(decision: &Decision) -> Result<(), String> {
    let allowed = match Effect::try_from(decision.effect) {
        Ok(Effect::Unspecified) => return Err("the decision sets no effect".into()),
        Ok(Effect::Allow) => true,
        Ok(Effect::Deny) => false,
        Err(_) => return Err(format!("unknown effect {}", decision.effect)),
    };
    if !allowed && !is_valid_reason_code(&decision.reason_code) {
        return Err(format!(
            "a deny needs a reason_code of 1 to 64 lowercase letters, digits or underscores, got {:?}",
            decision.reason_code
        ));
    }
    if decision.client_message.chars().count() > CLIENT_MESSAGE_MAX_CHARS {
        return Err(format!(
            "client_message is longer than {CLIENT_MESSAGE_MAX_CHARS} characters"
        ));
    }
    if decision.client_message.chars().any(char::is_control) {
        return Err("client_message contains control characters".into());
    }
    Ok(())
}

/// Pads `request` to `target` encoded bytes: a transaction request through a `raw` payload the
/// module cannot mistake for a real encoding, a registration request through its `request_id`.
fn at_size(mut request: HookRequest, target: usize) -> HookRequest {
    let pad = |request: &mut HookRequest, len: usize| match request.subject.as_mut() {
        Some(hook_request::Subject::Transaction(transaction)) => {
            transaction.raw = Some(RawTransaction {
                encoding: PADDING_ENCODING.into(),
                data: vec![0; len],
            });
        }
        _ => request.request_id = "r".repeat(len),
    };
    pad(&mut request, 0);
    let mut len = target.saturating_sub(prost::Message::encoded_len(&request));
    pad(&mut request, len);
    while prost::Message::encoded_len(&request) > target && len > 0 {
        len -= 1;
        pad(&mut request, len);
    }
    request
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::{ModuleError, ModuleInfo, TransactionSubject};

    fn transaction() -> HookRequest {
        HookRequest {
            params: b"{}".to_vec(),
            subject: Some(hook_request::Subject::Transaction(
                TransactionSubject::default(),
            )),
            ..Default::default()
        }
    }

    fn info() -> ModuleInfo {
        ModuleInfo {
            name: "test".into(),
            version: "0".into(),
            protocol_version: PROTOCOL_VERSION,
            hooks: vec![Hook::BeforeTransactionApproved as i32],
            ..Default::default()
        }
    }

    struct Fixed(Decision);

    #[async_trait]
    impl Module for Fixed {
        async fn describe(&self, _: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
            Ok(info())
        }

        async fn validate_params(&self, params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
            Ok(if params == b"{}" {
                ParamsVerdict::ok()
            } else {
                ParamsVerdict::invalid("only {}")
            })
        }

        async fn evaluate(&self, _: &HookRequest) -> Result<Decision, ModuleError> {
            Ok(self.0.clone())
        }
    }

    struct Counting(AtomicU64);

    #[async_trait]
    impl Module for Counting {
        async fn describe(&self, _: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
            Ok(info())
        }

        async fn validate_params(&self, _: &[u8]) -> Result<ParamsVerdict, ModuleError> {
            Ok(ParamsVerdict::ok())
        }

        async fn evaluate(&self, _: &HookRequest) -> Result<Decision, ModuleError> {
            if self.0.fetch_add(1, Ordering::SeqCst) < 2 {
                Ok(Decision::allow())
            } else {
                Ok(Decision::deny("limit", "over the limit"))
            }
        }
    }

    fn suite<M: Module + 'static>(module: M) -> Suite<M> {
        Suite::new(module)
            .denies("payment", transaction())
            .valid_params(json!({}))
            .invalid_params(json!({ "x": 1 }))
    }

    fn checks(report: &Report) -> Vec<&'static str> {
        let mut checks: Vec<_> = report.failures().iter().map(|f| f.check).collect();
        checks.dedup();
        checks
    }

    #[tokio::test]
    async fn a_well_behaved_module_conforms() {
        suite(Fixed(Decision::deny("blocked", "no")))
            .run()
            .await
            .assert_conformant();
    }

    #[tokio::test]
    async fn a_module_that_allows_everything_does_not_conform() {
        let always_allows = || Fixed(Decision::allow());
        let unstated = Suite::new(always_allows())
            .request("payment", transaction())
            .valid_params(json!({}))
            .invalid_params(json!({ "x": 1 }))
            .run()
            .await;
        assert_eq!(checks(&unstated), vec!["setup"], "{unstated}");

        let stated = suite(always_allows()).run().await;
        assert_eq!(checks(&stated), vec!["expectation"], "{stated}");
    }

    #[tokio::test]
    async fn each_expectation_is_checked() {
        let denies = || Fixed(Decision::deny("blocked", "no"));
        let wrong_code = Suite::new(denies())
            .denies_with("payment", transaction(), "amount_cap_exceeded")
            .valid_params(json!({}))
            .invalid_params(json!({ "x": 1 }))
            .run()
            .await;
        assert_eq!(checks(&wrong_code), vec!["expectation"], "{wrong_code}");

        let wrong_outcome = suite(denies())
            .allows("allowed payment", transaction())
            .fails("unjudgeable payment", transaction())
            .run()
            .await;
        assert_eq!(wrong_outcome.failures().len(), 2, "{wrong_outcome}");
        assert_eq!(
            checks(&wrong_outcome),
            vec!["expectation"],
            "{wrong_outcome}"
        );

        suite(denies())
            .denies_with("coded payment", transaction(), "blocked")
            .run()
            .await
            .assert_conformant();
    }

    #[tokio::test]
    async fn counting_in_evaluate_is_caught() {
        let report = suite(Counting(AtomicU64::new(0))).run().await;
        assert!(checks(&report).contains(&"purity"), "{report}");
    }

    #[tokio::test]
    async fn a_malformed_deny_is_caught() {
        let report = suite(Fixed(Decision::deny("Not Valid", "no"))).run().await;
        assert_eq!(checks(&report), vec!["decision"], "{report}");
        let long = Decision::deny("ok", "no").with_client_message("x".repeat(257));
        let report = suite(Fixed(long)).run().await;
        assert_eq!(checks(&report), vec!["decision"], "{report}");
    }

    #[tokio::test]
    async fn accepting_invalid_params_is_caught() {
        let report = Suite::new(Counting(AtomicU64::new(0)))
            .denies("payment", transaction())
            .valid_params(json!({}))
            .invalid_params(json!({ "x": 1 }))
            .run()
            .await;
        assert!(checks(&report).contains(&"params"), "{report}");
    }

    #[test]
    fn padding_reaches_the_cap() {
        for target in [4_096, DEFAULT_REQUEST_MAX_BYTES] {
            let padded = at_size(transaction(), target);
            let size = prost::Message::encoded_len(&padded);
            assert!(size <= target && size + 2 >= target, "{size} for {target}");
            let registration = at_size(HookRequest::default(), target);
            let size = prost::Message::encoded_len(&registration);
            assert!(size <= target && size + 2 >= target, "{size} for {target}");
        }
    }
}
