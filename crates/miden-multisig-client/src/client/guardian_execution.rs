//! Requesting and observing Guardian execution of this client's proposals.

use std::time::Duration;

use guardian_client::{ClientError, ProposalExecution};
use miden_protocol::account::AccountId;

use super::MultisigClient;
use crate::error::{MultisigError, Result};

const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(10);
const DEFAULT_WAIT_DEADLINE: Duration = Duration::from_secs(15 * 60);

fn refusal(error: ClientError) -> MultisigError {
    match error.guardian_code() {
        Some(code) => MultisigError::GuardianExecutionRefused {
            message: error.user_message().unwrap_or_else(|| error.to_string()),
            retryable: error.is_retryable(),
            retry_after: error.retry_after(),
            blocking_proposal_id: error.guardian_meta().and_then(|meta| {
                meta.get("blocking_proposal_id")
                    .and_then(|id| id.as_str())
                    .map(str::to_string)
            }),
            code,
        },
        None => MultisigError::from(error),
    }
}

/// How [`MultisigClient::wait_for_guardian_execution`] polls. The pause between polls starts at
/// `initial_backoff` and doubles after every poll up to `max_backoff`; a retryable read that
/// carries a server retry hint waits for the hint instead. The wait gives up once `deadline`
/// has elapsed since it started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionWaitOptions {
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub deadline: Duration,
}

impl Default for ExecutionWaitOptions {
    fn default() -> Self {
        Self {
            initial_backoff: DEFAULT_INITIAL_BACKOFF,
            max_backoff: DEFAULT_MAX_BACKOFF,
            deadline: DEFAULT_WAIT_DEADLINE,
        }
    }
}

/// A failed execution-status read, kept unmapped so the wait can decide whether to retry it.
enum StatusReadFailure {
    Connection(MultisigError),
    Guardian(ClientError),
}

/// What a failed status read asks the wait to do next.
enum ReadRetry {
    After(Duration),
    Backoff,
    Never,
}

impl StatusReadFailure {
    fn retry(&self) -> ReadRetry {
        match self {
            StatusReadFailure::Connection(_) => ReadRetry::Backoff,
            StatusReadFailure::Guardian(error) if error.is_retryable() || is_transport(error) => {
                error
                    .retry_after()
                    .map_or(ReadRetry::Backoff, ReadRetry::After)
            }
            StatusReadFailure::Guardian(_) => ReadRetry::Never,
        }
    }

    fn into_error(self) -> MultisigError {
        match self {
            StatusReadFailure::Connection(error) => error,
            StatusReadFailure::Guardian(error) => refusal(error),
        }
    }
}

/// A failure that never reached Guardian's handlers: the connection broke, or the transport
/// answered `Unavailable` or `DeadlineExceeded` without a Guardian error object.
fn is_transport(error: &ClientError) -> bool {
    match error {
        ClientError::Transport(_) => true,
        ClientError::Status(status) => {
            error.guardian_code().is_none()
                && matches!(
                    status.code(),
                    tonic::Code::Unavailable | tonic::Code::DeadlineExceeded
                )
        }
        ClientError::ServerError(_) | ClientError::Json(_) | ClientError::InvalidResponse(_) => {
            false
        }
    }
}

#[async_trait::async_trait]
trait WaitRuntime: Send + Sync {
    fn elapsed(&self) -> Duration;
    async fn sleep(&self, duration: Duration);
}

struct TokioWaitRuntime {
    started: tokio::time::Instant,
}

impl TokioWaitRuntime {
    fn start() -> Self {
        Self {
            started: tokio::time::Instant::now(),
        }
    }
}

#[async_trait::async_trait]
impl WaitRuntime for TokioWaitRuntime {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

#[async_trait::async_trait]
trait ExecutionStatusSource: Send {
    async fn read(&mut self) -> std::result::Result<ProposalExecution, StatusReadFailure>;
}

/// Reads one proposal's execution from Guardian, over a fresh authenticated connection each time.
struct GuardianStatusSource<'a> {
    client: &'a MultisigClient,
    account_id: AccountId,
    proposal_id: &'a str,
}

#[async_trait::async_trait]
impl ExecutionStatusSource for GuardianStatusSource<'_> {
    async fn read(&mut self) -> std::result::Result<ProposalExecution, StatusReadFailure> {
        let mut guardian = self
            .client
            .create_authenticated_guardian_client()
            .await
            .map_err(StatusReadFailure::Connection)?;
        guardian
            .get_delta_proposal_execution(&self.account_id, self.proposal_id)
            .await
            .map_err(StatusReadFailure::Guardian)
    }
}

/// Polls one proposal's execution until it is terminal or the deadline passes. It only reads:
/// it never asks Guardian to execute.
struct ExecutionWait<'a> {
    proposal_id: &'a str,
    options: ExecutionWaitOptions,
}

impl ExecutionWait<'_> {
    async fn run(
        &self,
        runtime: &dyn WaitRuntime,
        source: &mut dyn ExecutionStatusSource,
    ) -> Result<ProposalExecution> {
        let mut backoff = self.options.initial_backoff;
        let mut last_observed = None;
        loop {
            let pause = match source.read().await {
                Ok(execution) if execution.state.is_terminal() => return Ok(execution),
                Ok(execution) => {
                    last_observed = Some(execution);
                    backoff
                }
                Err(failure) => match failure.retry() {
                    ReadRetry::After(hint) => hint,
                    ReadRetry::Backoff => backoff,
                    ReadRetry::Never => return Err(failure.into_error()),
                },
            };
            let remaining = self.options.deadline.saturating_sub(runtime.elapsed());
            if remaining.is_zero() {
                return Err(self.timed_out(last_observed));
            }
            runtime.sleep(pause.min(remaining)).await;
            backoff = backoff.saturating_mul(2).min(self.options.max_backoff);
        }
    }

    fn timed_out(&self, last_observed: Option<ProposalExecution>) -> MultisigError {
        MultisigError::GuardianExecutionWaitTimedOut {
            proposal_id: self.proposal_id.to_string(),
            deadline: self.options.deadline,
            last_observed: last_observed.map(Box::new),
        }
    }
}

impl MultisigClient {
    /// Asks Guardian to prove and submit a threshold-met proposal. Returns once the request is
    /// accepted; [`execution_status`](Self::execution_status) reports the outcome. The proposal
    /// must have been created by a client in
    /// [`GuardianExecutable`](crate::ProposalExecutionMode::GuardianExecutable) mode.
    pub async fn request_guardian_execution(
        &mut self,
        proposal_id: &str,
    ) -> Result<ProposalExecution> {
        let account_id = self.require_account()?.id();
        let mut guardian = self.create_authenticated_guardian_client().await?;
        guardian
            .execute_delta_proposal(&account_id, proposal_id)
            .await
            .map_err(refusal)
    }

    /// The latest Guardian execution of a proposal.
    pub async fn execution_status(&mut self, proposal_id: &str) -> Result<ProposalExecution> {
        let account_id = self.require_account()?.id();
        let mut guardian = self.create_authenticated_guardian_client().await?;
        guardian
            .get_delta_proposal_execution(&account_id, proposal_id)
            .await
            .map_err(refusal)
    }

    /// The account's in-flight Guardian execution, if any.
    pub async fn current_execution(&mut self) -> Result<Option<ProposalExecution>> {
        let account_id = self.require_account()?.id();
        let mut guardian = self.create_authenticated_guardian_client().await?;
        guardian
            .get_current_execution(&account_id)
            .await
            .map_err(refusal)
    }

    /// Waits for a requested Guardian execution to finish and returns it once `committed` or
    /// `failed`. Status reads that fail with a retryable or transport error are retried; any
    /// other error is returned. Past `options.deadline` it returns
    /// [`GuardianExecutionWaitTimedOut`](MultisigError::GuardianExecutionWaitTimedOut). It never
    /// requests execution, so call
    /// [`request_guardian_execution`](Self::request_guardian_execution) first.
    pub async fn wait_for_guardian_execution(
        &mut self,
        proposal_id: &str,
        options: ExecutionWaitOptions,
    ) -> Result<ProposalExecution> {
        let account_id = self.require_account()?.id();
        let runtime = TokioWaitRuntime::start();
        let wait = ExecutionWait {
            proposal_id,
            options,
        };
        let mut source = GuardianStatusSource {
            client: self,
            account_id,
            proposal_id,
        };
        wait.run(&runtime, &mut source).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use guardian_client::ExecutionState;
    use guardian_client::execution::{ExecutionFailure, ExecutionFailureCode};

    use super::*;

    fn guardian_status(code: tonic::Code, details: serde_json::Value) -> ClientError {
        ClientError::from(tonic::Status::with_details(
            code,
            "refused",
            details.to_string().into_bytes().into(),
        ))
    }

    fn refused(details: serde_json::Value) -> MultisigError {
        refusal(guardian_status(tonic::Code::Aborted, details))
    }

    #[test]
    fn a_conflict_keeps_the_blocking_proposal_and_the_retry_hint() {
        let error = refused(serde_json::json!({
            "code": "GUARDIAN_EXECUTION_CONFLICT",
            "message": "another proposal is executing",
            "meta": { "retryable": false, "blocking_proposal_id": "0xabc" }
        }));
        let MultisigError::GuardianExecutionRefused {
            code,
            retryable,
            retry_after,
            blocking_proposal_id,
            ..
        } = error
        else {
            panic!("expected a refusal, got {error:?}");
        };
        assert_eq!(code, "GUARDIAN_EXECUTION_CONFLICT");
        assert!(!retryable);
        assert_eq!(retry_after, None);
        assert_eq!(blocking_proposal_id.as_deref(), Some("0xabc"));
    }

    #[test]
    fn a_busy_refusal_is_retryable_names_no_proposal_and_carries_the_retry_hint() {
        let error = refused(serde_json::json!({
            "code": "GUARDIAN_EXECUTION_BUSY",
            "message": "the account is busy",
            "meta": { "retryable": true, "retry_after_secs": 3 }
        }));
        let MultisigError::GuardianExecutionRefused {
            retryable,
            retry_after,
            blocking_proposal_id,
            ..
        } = error
        else {
            panic!("expected a refusal, got {error:?}");
        };
        assert!(retryable);
        assert_eq!(retry_after, Some(Duration::from_secs(3)));
        assert_eq!(blocking_proposal_id, None);
    }

    #[test]
    fn the_wait_defaults_are_one_second_ten_seconds_and_fifteen_minutes() {
        assert_eq!(
            ExecutionWaitOptions::default(),
            ExecutionWaitOptions {
                initial_backoff: Duration::from_secs(1),
                max_backoff: Duration::from_secs(10),
                deadline: Duration::from_secs(900),
            }
        );
    }

    #[derive(Default)]
    struct FakeRuntime {
        now: Mutex<Duration>,
        sleeps: Mutex<Vec<Duration>>,
    }

    #[async_trait::async_trait]
    impl WaitRuntime for FakeRuntime {
        fn elapsed(&self) -> Duration {
            *self.now.lock().unwrap()
        }

        async fn sleep(&self, duration: Duration) {
            *self.now.lock().unwrap() += duration;
            self.sleeps.lock().unwrap().push(duration);
        }
    }

    impl FakeRuntime {
        fn sleeps_secs(&self) -> Vec<u64> {
            self.sleeps
                .lock()
                .unwrap()
                .iter()
                .map(Duration::as_secs)
                .collect()
        }
    }

    fn execution(state: ExecutionState) -> ProposalExecution {
        ProposalExecution {
            account_id: "0xacc".to_string(),
            proposal_id: "0xprop".to_string(),
            state,
            error: (state == ExecutionState::Failed).then(|| ExecutionFailure {
                code: ExecutionFailureCode::ProvingFailed,
                message: "proving failed".to_string(),
            }),
            delta_nonce: None,
            newly_accepted: false,
            proposal_exists: true,
            ignored_signatures: 0,
            updated_at: "2026-10-02T00:00:00Z".to_string(),
        }
    }

    type Read = std::result::Result<ProposalExecution, StatusReadFailure>;

    struct ScriptedSource {
        script: VecDeque<Read>,
        reads: usize,
    }

    #[async_trait::async_trait]
    impl ExecutionStatusSource for ScriptedSource {
        async fn read(&mut self) -> Read {
            self.reads += 1;
            self.script
                .pop_front()
                .unwrap_or_else(|| Ok(execution(ExecutionState::Pending)))
        }
    }

    fn options(initial: u64, max: u64, deadline: u64) -> ExecutionWaitOptions {
        ExecutionWaitOptions {
            initial_backoff: Duration::from_secs(initial),
            max_backoff: Duration::from_secs(max),
            deadline: Duration::from_secs(deadline),
        }
    }

    async fn wait(
        runtime: &FakeRuntime,
        options: ExecutionWaitOptions,
        reads: Vec<Read>,
    ) -> (Result<ProposalExecution>, usize) {
        let mut source = ScriptedSource {
            script: VecDeque::from(reads),
            reads: 0,
        };
        let wait = ExecutionWait {
            proposal_id: "0xprop",
            options,
        };
        let outcome = wait.run(runtime, &mut source).await;
        (outcome, source.reads)
    }

    #[tokio::test]
    async fn returns_a_committed_execution_after_backing_off_between_polls() {
        let runtime = FakeRuntime::default();
        let (outcome, reads) = wait(
            &runtime,
            options(1, 10, 900),
            vec![
                Ok(execution(ExecutionState::Pending)),
                Ok(execution(ExecutionState::Proving)),
                Ok(execution(ExecutionState::Submitted)),
                Ok(execution(ExecutionState::Committed)),
            ],
        )
        .await;
        assert_eq!(outcome.unwrap().state, ExecutionState::Committed);
        assert_eq!(reads, 4);
        assert_eq!(runtime.sleeps_secs(), vec![1, 2, 4]);
    }

    #[tokio::test]
    async fn returns_a_failed_execution_without_waiting() {
        let runtime = FakeRuntime::default();
        let (outcome, reads) = wait(
            &runtime,
            options(1, 10, 900),
            vec![Ok(execution(ExecutionState::Failed))],
        )
        .await;
        let execution = outcome.unwrap();
        assert_eq!(execution.state, ExecutionState::Failed);
        assert!(execution.error.is_some());
        assert_eq!(reads, 1);
        assert!(runtime.sleeps_secs().is_empty());
    }

    #[tokio::test]
    async fn retries_transient_reads_and_honours_the_retry_hint() {
        let runtime = FakeRuntime::default();
        let (outcome, reads) = wait(
            &runtime,
            options(1, 10, 900),
            vec![
                Err(StatusReadFailure::Guardian(ClientError::from(
                    tonic::Status::new(tonic::Code::Unavailable, "connection reset"),
                ))),
                Err(StatusReadFailure::Guardian(guardian_status(
                    tonic::Code::ResourceExhausted,
                    serde_json::json!({
                        "code": "rate_limit_exceeded",
                        "message": "slow down",
                        "meta": { "retryable": true, "retry_after_secs": 7 }
                    }),
                ))),
                Err(StatusReadFailure::Connection(
                    MultisigError::GuardianConnection("refused".to_string()),
                )),
                Ok(execution(ExecutionState::Committed)),
            ],
        )
        .await;
        assert_eq!(outcome.unwrap().state, ExecutionState::Committed);
        assert_eq!(reads, 4);
        assert_eq!(runtime.sleeps_secs(), vec![1, 7, 4]);
    }

    #[tokio::test]
    async fn returns_a_non_retryable_read_error() {
        let runtime = FakeRuntime::default();
        let (outcome, reads) = wait(
            &runtime,
            options(1, 10, 900),
            vec![Err(StatusReadFailure::Guardian(guardian_status(
                tonic::Code::NotFound,
                serde_json::json!({
                    "code": "GUARDIAN_EXECUTION_NOT_FOUND",
                    "message": "not asked",
                    "meta": { "retryable": false }
                }),
            )))],
        )
        .await;
        let Err(MultisigError::GuardianExecutionRefused { code, .. }) = outcome else {
            panic!("expected a refusal, got {outcome:?}");
        };
        assert_eq!(code, "GUARDIAN_EXECUTION_NOT_FOUND");
        assert_eq!(reads, 1);
    }

    #[tokio::test]
    async fn times_out_at_the_deadline_with_the_last_observed_execution() {
        let runtime = FakeRuntime::default();
        let (outcome, reads) = wait(
            &runtime,
            options(1, 2, 5),
            vec![Ok(execution(ExecutionState::Proving))],
        )
        .await;
        let Err(MultisigError::GuardianExecutionWaitTimedOut {
            proposal_id,
            deadline,
            last_observed,
        }) = outcome
        else {
            panic!("expected a timeout, got {outcome:?}");
        };
        assert_eq!(proposal_id, "0xprop");
        assert_eq!(deadline, Duration::from_secs(5));
        assert_eq!(
            last_observed.map(|execution| execution.state),
            Some(ExecutionState::Pending)
        );
        assert_eq!(reads, 4);
        assert_eq!(runtime.sleeps_secs(), vec![1, 2, 2]);
    }

    #[tokio::test]
    async fn times_out_without_an_observation_when_every_read_fails() {
        let runtime = FakeRuntime::default();
        let connection_refused = || {
            Err(StatusReadFailure::Connection(
                MultisigError::GuardianConnection("refused".to_string()),
            ))
        };
        let mut source = ScriptedSource {
            script: VecDeque::from([
                connection_refused(),
                connection_refused(),
                connection_refused(),
            ]),
            reads: 0,
        };
        let wait = ExecutionWait {
            proposal_id: "0xprop",
            options: options(1, 1, 2),
        };
        let outcome = wait.run(&runtime, &mut source).await;
        let Err(MultisigError::GuardianExecutionWaitTimedOut { last_observed, .. }) = outcome
        else {
            panic!("expected a timeout, got {outcome:?}");
        };
        assert!(last_observed.is_none());
        assert_eq!(source.reads, 3);
    }

    #[allow(dead_code)]
    fn the_wait_future_is_send(client: &mut MultisigClient) {
        fn assert_send<T: Send>(_: T) {}
        assert_send(client.wait_for_guardian_execution("0xprop", ExecutionWaitOptions::default()));
    }
}
