pub mod miden;

pub use guardian_shared::retry::RpcReadMode;

use crate::error::GuardianError;
use crate::metadata::auth::{Auth, Credentials};
use async_trait::async_trait;
use std::sync::{Arc, LazyLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Conservative process-wide cap on concurrent state reconstructions.
///
/// State reconstruction (`apply_delta` / `verify_delta`) is synchronous,
/// CPU-bound work whose cost grows with account storage size — the
/// `executed_transactions` replay map is never pruned, so it grows unbounded
/// with account history. The `apply_delta` microbenchmark
/// (`crate::testing::apply_delta_bench`) measures ~1 ms for a fresh account
/// rising to ~1 s for a large, long-lived one. Without a shared bound, a
/// canonicalization backlog (up to `max_concurrent_accounts` in flight) or a
/// burst of concurrent `push_delta` requests could run many such jobs at once,
/// contend for host CPU, and degrade API latency on the leader replica.
///
/// This bound protects runtime responsiveness; it does not reduce the
/// O(account-size) reconstruction work or repeated full-state passes tracked in
/// [issue #328](https://github.com/OpenZeppelin/guardian/issues/328).
///
/// The capacity is half the host's cores (minimum 1): reconstructions may use
/// at most half the machine, leaving the rest for the async runtime, signature
/// verification, and storage I/O, and concurrent `push_delta` requests don't
/// serialize behind a single slow (large-account) reconstruction on hosts with
/// more than two cores. Background work is further capped at one slot by the
/// [`Reconstructor::run_background`] admission gate. A runtime-safety bound
/// derived from the host, not operator policy, so deliberately not
/// configurable.
fn max_concurrent_reconstructions() -> usize {
    std::thread::available_parallelism()
        .map(|cores| cores.get() / 2)
        .unwrap_or(1)
        .max(1)
}

/// Failure of a reconstruction dispatched through [`Reconstructor::run`].
/// Kept distinct from [`GuardianError`] so the caller preserves the split:
/// the operation itself failing is the client's fault, the task failing to run
/// is the server's.
#[derive(Debug)]
pub enum ReconstructError {
    /// The reconstruction operation (`apply_delta` / `verify_delta`) returned
    /// an error — an invalid delta, i.e. a client fault.
    Operation(String),
    /// The blocking task did not run to completion (panic or runtime
    /// shutdown) — a server fault, never the client's.
    Task(String),
}

impl From<ReconstructError> for GuardianError {
    fn from(error: ReconstructError) -> Self {
        match error {
            ReconstructError::Operation(message) => GuardianError::InvalidDelta(message),
            ReconstructError::Task(message) => {
                GuardianError::StorageError(format!("state reconstruction failed: {message}"))
            }
        }
    }
}

/// A process-wide bound on concurrent, CPU-bound state reconstructions, shared
/// by the canonicalization worker and the `push_delta` API path so neither can
/// exceed the limit — individually or in aggregate.
///
/// Two gates. `cpu` is the real CPU bound both paths pass through. `admission`
/// is a background-only gate the worker takes *before* `cpu`: because Tokio
/// semaphores are fair (FIFO), account tasks running with
/// `buffer_unordered(max_concurrent_accounts)` could otherwise queue many
/// reconstructions on the `cpu` gate, and a `push_delta` arriving after them
/// would wait behind the whole backlog. With `admission` capacity 1, only the
/// active background reconstruction can hold or wait for `cpu`.
#[derive(Clone)]
pub struct Reconstructor {
    cpu: Arc<Semaphore>,
    admission: Arc<Semaphore>,
}

impl Reconstructor {
    fn with_capacity(cpu_permits: usize) -> Self {
        Self {
            cpu: Arc::new(Semaphore::new(cpu_permits)),
            admission: Arc::new(Semaphore::new(1)),
        }
    }

    /// API path (`push_delta`): straight to the shared CPU gate.
    pub async fn run<T, F>(&self, op: F) -> Result<T, ReconstructError>
    where
        F: FnOnce() -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        self.dispatch(None, op).await
    }

    /// Background path (canonicalization worker): take the background admission
    /// gate before the shared CPU gate. Both permits move into the blocking
    /// closure, so cancellation cannot admit another background reconstruction
    /// while detached work is still using the CPU slot.
    pub async fn run_background<T, F>(&self, op: F) -> Result<T, ReconstructError>
    where
        F: FnOnce() -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let admission = self
            .admission
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| ReconstructError::Task(format!("admission gate closed: {e}")))?;
        self.dispatch(Some(admission), op).await
    }

    async fn dispatch<T, F>(
        &self,
        admission: Option<OwnedSemaphorePermit>,
        op: F,
    ) -> Result<T, ReconstructError>
    where
        F: FnOnce() -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let cpu = self
            .cpu
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| ReconstructError::Task(format!("reconstruction gate closed: {e}")))?;

        match tokio::task::spawn_blocking(move || {
            let _admission = admission;
            let _cpu = cpu;
            op()
        })
        .await
        {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(operation_error)) => Err(ReconstructError::Operation(operation_error)),
            Err(join_error) => {
                tracing::error!(error = %join_error, "state reconstruction task failed to complete");
                Err(ReconstructError::Task(join_error.to_string()))
            }
        }
    }

    #[cfg(test)]
    fn available_cpu_permits(&self) -> usize {
        self.cpu.available_permits()
    }

    #[cfg(test)]
    fn available_admission_permits(&self) -> usize {
        self.admission.available_permits()
    }
}

/// The one shared reconstruction gate for the whole process.
pub fn reconstructor() -> &'static Reconstructor {
    static INSTANCE: LazyLock<Reconstructor> =
        LazyLock::new(|| Reconstructor::with_capacity(max_concurrent_reconstructions()));
    &INSTANCE
}

/// Outcome of comparing a locally-computed state commitment against the
/// on-chain one. A mismatch is a legitimate observation (the tx has not
/// landed yet, or the account advanced past the expected state), not an
/// error: `Err` from [`NetworkClient::verify_commitment`] is reserved for
/// failures to make the comparison at all, such as an RPC failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateVerification {
    /// The on-chain commitment equals the locally-computed one.
    Match,
    /// The chain has no state for this account at all — its first
    /// transaction has not landed yet. How absence is detected is
    /// network-specific and owned by the [`NetworkClient`] impl; callers
    /// never see the raw sentinel the chain reports for unknown accounts.
    Absent,
    /// The on-chain commitment differs; carries the observed on-chain
    /// value so callers can classify the mismatch.
    Mismatch { on_chain: String },
}

#[async_trait]
pub trait NetworkClient: Send + Sync {
    /// Get state commitment in hex format from JSON
    fn get_state_commitment(
        &self,
        account_id: &str,
        state_json: &serde_json::Value,
    ) -> Result<String, String>;

    /// Compare an expected commitment against the on-chain account commitment.
    /// Returns `Err` only when the comparison could not be made.
    ///
    /// Canonicalization call sites must pass [`RpcReadMode::SingleAttempt`]:
    /// the pass holds a lease and retries structurally on its schedule, so a
    /// failed read is one missed observation, recovered by the next pass —
    /// never by re-querying the same endpoint within the pass.
    async fn verify_commitment(
        &self,
        account_id: &str,
        expected_commitment: &str,
        read_mode: RpcReadMode,
    ) -> Result<StateVerification, String>;

    /// Verify delta is valid for given state
    fn verify_delta(
        &self,
        prev_proof: &str,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(), String>;

    /// Apply delta to state
    fn apply_delta(
        &self,
        prev_state_json: &serde_json::Value,
        delta_payload: &serde_json::Value,
    ) -> Result<(serde_json::Value, String), String>;

    /// Merge multiple deltas
    fn merge_deltas(
        &self,
        delta_payloads: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String>;

    /// Get delta proposal ID
    fn delta_proposal_id(
        &self,
        account_id: &str,
        nonce: u64,
        delta_payload: &serde_json::Value,
    ) -> Result<String, String>;

    /// Validate account ID format
    fn validate_account_id(&self, account_id: &str) -> Result<(), String>;

    /// Validate that the credential (public key) is authorized for the account
    /// Checks storage slot 0 (single signer) or slot 1 (mapping of cosigners)
    fn validate_credential(
        &self,
        state_json: &serde_json::Value,
        credential: &Credentials,
        auth: &Auth,
    ) -> Result<(), String>;

    /// Validate that account storage is bound to this server's GUARDIAN public key commitment.
    fn validate_guardian_commitment(
        &self,
        state_json: &serde_json::Value,
        expected_guardian_commitment: &str,
    ) -> Result<(), String>;

    /// Extract the guardian public key commitment stored in the account
    /// state, or `Ok(None)` when the state carries no guardian binding
    /// (non-guardian account types, networks without the concept). Used
    /// by the release-on-guardian-switch hook to detect that a committed
    /// delta moved the account to a different guardian. The default is
    /// `Ok(None)` — "cannot tell" — so backends without guardian storage
    /// never trigger a release.
    fn extract_guardian_commitment(
        &self,
        state_json: &serde_json::Value,
    ) -> Result<Option<String>, String> {
        let _ = state_json;
        Ok(None)
    }

    /// Determine if account auth should be updated given the state
    async fn should_update_auth(
        &self,
        state_json: &serde_json::Value,
        current_auth: &Auth,
    ) -> Result<Option<Auth>, String>;
}

/// Network type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum NetworkType {
    MidenTestnet,
    MidenDevnet,
    #[default]
    MidenLocal,
}

impl NetworkType {
    const ACCEPTED_VALUES: &str =
        "MidenLocal (local), MidenTestnet (testnet), MidenDevnet (devnet); case-insensitive";

    pub fn from_env(var_name: &str) -> Result<Self, String> {
        match std::env::var(var_name) {
            Ok(value) => Self::from_name(&value).ok_or_else(|| {
                format!(
                    "{var_name} has unrecognized value \"{value}\"; accepted values: {}",
                    Self::ACCEPTED_VALUES
                )
            }),
            Err(std::env::VarError::NotPresent) => Err(format!(
                "{var_name} is not set; accepted values: {}",
                Self::ACCEPTED_VALUES
            )),
            Err(std::env::VarError::NotUnicode(_)) => Err(format!(
                "{var_name} contains non-Unicode data; accepted values: {}",
                Self::ACCEPTED_VALUES
            )),
        }
    }

    pub fn from_name(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "midenlocal" | "local" => Some(Self::MidenLocal),
            "midentestnet" | "testnet" => Some(Self::MidenTestnet),
            "midendevnet" | "devnet" => Some(Self::MidenDevnet),
            _ => None,
        }
    }

    pub fn rpc_endpoint(&self) -> &str {
        match self {
            NetworkType::MidenTestnet => "https://rpc.testnet.miden.io",
            NetworkType::MidenDevnet => "https://rpc.devnet.miden.io",
            NetworkType::MidenLocal => "http://localhost:57291",
        }
    }
}

/// Miden node RPC settings resolved from the environment on top of the
/// declared [`NetworkType`]. The endpoint override changes only the transport
/// target — network identity (bech32 prefixes, dashboard rendering) always
/// derives from the network type.
/// Node RPC settings keyed by the network they configure. Only Miden is
/// designed today; future networks add a variant here without reshaping the
/// builder surface.
#[derive(Debug)]
#[non_exhaustive]
pub enum RpcSettings {
    Miden(MidenRpcSettings),
}

impl RpcSettings {
    /// Resolves node RPC settings from the environment for the declared
    /// network type. The match is the one place that maps a network to its
    /// settings family; a future network type extends it here.
    pub fn from_env(network: NetworkType) -> Result<Self, String> {
        match network {
            NetworkType::MidenTestnet | NetworkType::MidenDevnet | NetworkType::MidenLocal => {
                Ok(Self::Miden(MidenRpcSettings::from_env(network)?))
            }
        }
    }
}

#[derive(Debug)]
pub struct MidenRpcSettings {
    endpoint: crate::secret::CredentialUrl,
    endpoint_overridden: bool,
    network: NetworkType,
    timeout_ms: u32,
    max_attempts: u32,
}

impl MidenRpcSettings {
    pub const ENDPOINT_ENV: &str = "GUARDIAN_MIDEN_RPC_ENDPOINT";
    pub const TIMEOUT_ENV: &str = "GUARDIAN_MIDEN_RPC_TIMEOUT_MS";
    pub const MAX_ATTEMPTS_ENV: &str = "GUARDIAN_MIDEN_RPC_MAX_ATTEMPTS";
    pub const DEFAULT_TIMEOUT_MS: u32 = 10_000;

    pub fn from_env(network: NetworkType) -> Result<Self, String> {
        let endpoint_override = match std::env::var(Self::ENDPOINT_ENV) {
            Ok(value) => Some(crate::secret::CredentialUrl::new(value)),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(format!("{} contains non-Unicode data", Self::ENDPOINT_ENV));
            }
        };
        let timeout_ms = positive_u32_from_env(Self::TIMEOUT_ENV, Self::DEFAULT_TIMEOUT_MS)?;
        let max_attempts = positive_u32_from_env(Self::MAX_ATTEMPTS_ENV, 1)?;
        Self::resolve(network, endpoint_override, timeout_ms, max_attempts)
    }

    fn resolve(
        network: NetworkType,
        endpoint_override: Option<crate::secret::CredentialUrl>,
        timeout_ms: u32,
        max_attempts: u32,
    ) -> Result<Self, String> {
        match endpoint_override {
            Some(value) => {
                let trimmed = value.expose_secret().trim();
                validate_rpc_endpoint(trimmed)
                    .map_err(|rule| format!("{}: {rule}", Self::ENDPOINT_ENV))?;
                Ok(Self {
                    endpoint: crate::secret::CredentialUrl::new(trimmed.to_owned()),
                    endpoint_overridden: true,
                    network,
                    timeout_ms,
                    max_attempts,
                })
            }
            None => Ok(Self {
                endpoint: crate::secret::CredentialUrl::new(network.rpc_endpoint().to_owned()),
                endpoint_overridden: false,
                network,
                timeout_ms,
                max_attempts,
            }),
        }
    }

    pub(crate) fn endpoint(&self) -> &crate::secret::CredentialUrl {
        &self.endpoint
    }

    pub(crate) fn network(&self) -> NetworkType {
        self.network
    }

    pub fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(u64::from(self.timeout_ms))
    }

    pub fn read_retry_policy(&self) -> guardian_shared::retry::RetryPolicy {
        guardian_shared::retry::RetryPolicy::new(self.max_attempts)
    }

    pub fn client_settings(&self) -> miden_rpc_client::RpcClientSettings {
        miden_rpc_client::RpcClientSettings::new(self.timeout(), self.read_retry_policy())
    }

    /// `scheme://host[:port]` only — the full URL may carry credentials.
    pub fn sanitized_endpoint(&self) -> String {
        self.endpoint.scheme_and_host()
    }

    /// An override pointed at a public network identity is legitimate for a
    /// mirror and a mistake otherwise; callers log a startup warning.
    pub fn overrides_public_network(&self) -> bool {
        self.endpoint_overridden
            && matches!(
                self.network,
                NetworkType::MidenTestnet | NetworkType::MidenDevnet
            )
    }
}

fn positive_u32_from_env(key: &str, default: u32) -> Result<u32, String> {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().parse::<u32>() {
            Ok(0) => Err(format!("{key} must be a positive integer, got 0")),
            Ok(value) => Ok(value),
            Err(_) => Err(format!("{key} must be a positive integer, got {raw:?}")),
        },
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(std::env::VarError::NotUnicode(_)) => Err(format!("{key} must contain valid UTF-8")),
    }
}

/// The rule text never echoes the value: the URL may embed credentials.
fn validate_rpc_endpoint(value: &str) -> Result<(), String> {
    let rule = "must be an absolute HTTP(S) URL with a host".to_string();
    let url = url::Url::parse(value).map_err(|_| rule.clone())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(rule);
    }
    Ok(())
}

impl std::fmt::Display for NetworkType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetworkType::MidenTestnet => write!(f, "MidenTestnet"),
            NetworkType::MidenDevnet => write!(f, "MidenDevnet"),
            NetworkType::MidenLocal => write!(f, "MidenLocal"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MidenRpcSettings;
    use super::{
        GuardianError, NetworkType, ReconstructError, Reconstructor,
        max_concurrent_reconstructions, reconstructor,
    };
    use crate::testing::env_lock::ENV_LOCK;
    use axum::http::StatusCode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn process_wide_gate_capacity_is_half_the_cores_with_a_floor_of_one() {
        let capacity = max_concurrent_reconstructions();
        let cores = std::thread::available_parallelism()
            .expect("test host reports its parallelism")
            .get();
        assert_eq!(capacity, (cores / 2).max(1));
    }

    #[tokio::test]
    async fn both_paths_share_the_one_process_wide_reconstructor() {
        assert!(
            std::ptr::eq(reconstructor(), reconstructor()),
            "the API and worker paths resolve to the same gate instance",
        );
    }

    #[tokio::test]
    async fn capacity_one_serializes_concurrent_reconstructions() {
        let gate = Reconstructor::with_capacity(1);
        assert_eq!(peak_concurrency(&gate).await, 1);
    }

    #[tokio::test]
    async fn capacity_two_allows_overlap_proving_the_gate_is_the_limiter() {
        let gate = Reconstructor::with_capacity(2);
        assert_eq!(peak_concurrency(&gate).await, 2);
    }

    /// Run two reconstructions concurrently through `gate` and report the peak
    /// observed overlap. Each op holds for a beat so a permitted overlap is
    /// actually witnessed.
    async fn peak_concurrency(gate: &Reconstructor) -> usize {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let op = |live: Arc<AtomicUsize>, peak: Arc<AtomicUsize>| {
            move || {
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(50));
                live.fetch_sub(1, Ordering::SeqCst);
                Ok::<(), String>(())
            }
        };
        let (a, b) = tokio::join!(
            gate.run(op(live.clone(), peak.clone())),
            gate.run(op(live.clone(), peak.clone())),
        );
        a.unwrap();
        b.unwrap();
        peak.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn dropped_caller_holds_permit_until_blocking_work_completes() {
        use tokio::sync::oneshot;

        let gate = Reconstructor::with_capacity(1);
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();

        let handle = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.run(move || {
                    let _ = started_tx.send(());
                    release_rx.blocking_recv().ok();
                    Ok::<(), String>(())
                })
                .await
            }
        });

        started_rx.await.expect("blocking op started");
        handle.abort();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            gate.available_cpu_permits(),
            0,
            "the detached blocking op must keep the permit after the caller is dropped",
        );

        release_tx.send(()).expect("release the blocking op");
        for _ in 0..200 {
            if gate.available_cpu_permits() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            gate.available_cpu_permits(),
            1,
            "the permit is released once the blocking op finishes",
        );
    }

    #[tokio::test]
    async fn dropped_background_caller_holds_admission_until_blocking_work_completes() {
        use tokio::sync::oneshot;

        let gate = Reconstructor::with_capacity(1);
        let second_started = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (release_tx, release_rx) = oneshot::channel::<()>();

        let first = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.run_background(move || {
                    let _ = started_tx.send(());
                    release_rx.blocking_recv().ok();
                    Ok::<(), String>(())
                })
                .await
            }
        });

        started_rx.await.expect("background op started");
        first.abort();
        let _ = first.await;
        assert_eq!(
            gate.available_admission_permits(),
            0,
            "detached background work must retain admission after caller cancellation",
        );

        let second = tokio::spawn({
            let gate = gate.clone();
            let second_started = second_started.clone();
            async move {
                gate.run_background(move || {
                    second_started.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), String>(())
                })
                .await
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            second_started.load(Ordering::SeqCst),
            0,
            "another background op must not be admitted while detached work is active",
        );

        release_tx.send(()).expect("release the blocking op");
        second
            .await
            .expect("second task joins")
            .expect("second op runs");
        assert_eq!(second_started.load(Ordering::SeqCst), 1);
        assert_eq!(gate.available_admission_permits(), 1);
    }

    #[tokio::test]
    async fn api_push_is_served_before_a_queued_canonicalization_backlog() {
        use std::sync::Mutex;
        use tokio::sync::oneshot;

        let gate = Reconstructor::with_capacity(1);
        let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

        let (holder_started_tx, holder_started_rx) = oneshot::channel::<()>();
        let (release_holder_tx, release_holder_rx) = oneshot::channel::<()>();

        let holder = tokio::spawn({
            let gate = gate.clone();
            let order = order.clone();
            async move {
                gate.run_background(move || {
                    order.lock().unwrap().push("bg-holder");
                    let _ = holder_started_tx.send(());
                    release_holder_rx.blocking_recv().ok();
                    Ok::<(), String>(())
                })
                .await
                .unwrap();
            }
        });
        holder_started_rx
            .await
            .expect("holder occupies the CPU gate");

        let mut backlog = Vec::new();
        for _ in 0..5 {
            let gate = gate.clone();
            let order = order.clone();
            backlog.push(tokio::spawn(async move {
                gate.run_background(move || {
                    order.lock().unwrap().push("bg");
                    Ok::<(), String>(())
                })
                .await
                .unwrap();
            }));
        }

        let api = tokio::spawn({
            let gate = gate.clone();
            let order = order.clone();
            async move {
                gate.run(move || {
                    order.lock().unwrap().push("api");
                    Ok::<(), String>(())
                })
                .await
                .unwrap();
            }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        release_holder_tx.send(()).expect("release the holder");

        holder.await.unwrap();
        api.await.unwrap();
        for task in backlog {
            task.await.unwrap();
        }

        let order = order.lock().unwrap();
        assert_eq!(
            order[1], "api",
            "the API push runs right after the executing reconstruction, ahead of the backlog: {order:?}",
        );
    }

    #[tokio::test]
    async fn operation_error_is_a_client_fault() {
        let gate = Reconstructor::with_capacity(1);
        let error = gate
            .run(|| Err::<(), String>("bad delta".to_string()))
            .await
            .expect_err("operation error surfaces");
        assert!(matches!(error, ReconstructError::Operation(_)));
        assert_eq!(
            GuardianError::from(error).http_status(),
            StatusCode::BAD_REQUEST,
        );
    }

    #[tokio::test]
    async fn task_panic_is_a_server_fault() {
        let gate = Reconstructor::with_capacity(1);
        let error = gate
            .run(|| -> Result<(), String> { panic!("reconstruction boom") })
            .await
            .expect_err("a panic surfaces as a task failure");
        assert!(matches!(error, ReconstructError::Task(_)));
        assert_eq!(
            GuardianError::from(error).http_status(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[test]
    fn from_env_errors_when_var_missing() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_NETWORK_TYPE_TEST_MISSING";
        unsafe { std::env::remove_var(var_name) };

        let error = NetworkType::from_env(var_name).unwrap_err();

        assert!(error.contains(var_name));
        assert!(error.contains("MidenLocal"));
        assert!(error.contains("MidenTestnet"));
        assert!(error.contains("MidenDevnet"));
    }

    #[test]
    fn from_env_errors_when_value_unrecognized() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_NETWORK_TYPE_TEST_INVALID";
        unsafe { std::env::set_var(var_name, "tesnet") };

        let error = NetworkType::from_env(var_name).unwrap_err();

        assert!(error.contains(var_name));
        assert!(error.contains("tesnet"));
        assert!(error.contains("MidenTestnet"));
        unsafe { std::env::remove_var(var_name) };
    }

    #[cfg(unix)]
    #[test]
    fn from_env_errors_when_value_is_not_unicode() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let var_name = "GUARDIAN_NETWORK_TYPE_TEST_NOT_UNICODE";
        unsafe { std::env::set_var(var_name, OsString::from_vec(vec![0xff])) };

        let error = NetworkType::from_env(var_name).unwrap_err();

        assert!(error.contains(var_name));
        assert!(error.contains("non-Unicode"));
        unsafe { std::env::remove_var(var_name) };
    }

    #[test]
    fn from_env_parses_every_accepted_spelling() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let cases = [
            ("MidenLocal", NetworkType::MidenLocal),
            ("local", NetworkType::MidenLocal),
            ("LOCAL", NetworkType::MidenLocal),
            ("MidenTestnet", NetworkType::MidenTestnet),
            ("testnet", NetworkType::MidenTestnet),
            ("MidenDevnet", NetworkType::MidenDevnet),
            ("devnet", NetworkType::MidenDevnet),
            ("DEVNET", NetworkType::MidenDevnet),
        ];

        let var_name = "GUARDIAN_NETWORK_TYPE_TEST_PRESENT";
        for (value, expected) in cases {
            unsafe { std::env::set_var(var_name, value) };
            assert_eq!(NetworkType::from_env(var_name).unwrap(), expected);
        }
        unsafe { std::env::remove_var(var_name) };
    }

    #[test]
    fn rpc_settings_default_to_the_network_endpoint() {
        let settings = MidenRpcSettings::resolve(NetworkType::MidenLocal, None, 30_000, 1).unwrap();
        assert_eq!(
            settings.endpoint().expose_secret(),
            "http://localhost:57291"
        );
        assert!(!settings.overrides_public_network());
    }

    #[test]
    fn rpc_settings_accept_a_valid_override() {
        let settings = MidenRpcSettings::resolve(
            NetworkType::MidenLocal,
            Some(crate::secret::CredentialUrl::new(
                " http://node-sidecar:57291 ".to_string(),
            )),
            30_000,
            1,
        )
        .unwrap();
        assert_eq!(
            settings.endpoint().expose_secret(),
            "http://node-sidecar:57291"
        );
        assert!(!settings.overrides_public_network());
    }

    #[test]
    fn rpc_settings_reject_invalid_overrides_without_echoing_the_value() {
        for value in [
            "localhost:57291",
            "rpc.example",
            "ftp://rpc.example",
            "https://",
            "",
        ] {
            let error = MidenRpcSettings::resolve(
                NetworkType::MidenLocal,
                Some(crate::secret::CredentialUrl::new(value.to_string())),
                30_000,
                1,
            )
            .unwrap_err();
            assert!(error.contains("GUARDIAN_MIDEN_RPC_ENDPOINT"), "{error}");
            assert!(error.contains("absolute HTTP(S) URL"), "{error}");
            if !value.is_empty() {
                assert!(
                    !error.contains(value),
                    "error must not echo the value: {error}"
                );
            }
        }
    }

    #[test]
    fn rpc_settings_flag_public_network_overrides() {
        for network in [NetworkType::MidenTestnet, NetworkType::MidenDevnet] {
            let settings = MidenRpcSettings::resolve(
                network,
                Some(crate::secret::CredentialUrl::new(
                    "https://mirror.internal".to_string(),
                )),
                30_000,
                1,
            )
            .unwrap();
            assert!(settings.overrides_public_network());
        }
    }

    #[test]
    fn rpc_settings_sanitize_credentials_out_of_the_loggable_endpoint() {
        let settings = MidenRpcSettings::resolve(
            NetworkType::MidenLocal,
            Some(crate::secret::CredentialUrl::new(
                "https://user:s3cret@mirror.internal:8443/rpc?key=abc".to_string(),
            )),
            30_000,
            1,
        )
        .unwrap();
        let sanitized = settings.sanitized_endpoint();
        assert_eq!(sanitized, "https://mirror.internal:8443");
        assert!(!sanitized.contains("s3cret"));
        assert!(!sanitized.contains("key=abc"));
    }

    #[test]
    fn rpc_settings_from_env_reads_the_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let var = MidenRpcSettings::ENDPOINT_ENV;
        unsafe { std::env::set_var(var, "http://node-sidecar:57291") };
        let settings = MidenRpcSettings::from_env(NetworkType::MidenLocal).unwrap();
        unsafe { std::env::remove_var(var) };
        assert_eq!(
            settings.endpoint().expose_secret(),
            "http://node-sidecar:57291"
        );
    }

    #[test]
    fn rpc_settings_parse_timeout_and_attempts_with_defaults() {
        let settings = MidenRpcSettings::resolve(
            NetworkType::MidenLocal,
            None,
            MidenRpcSettings::DEFAULT_TIMEOUT_MS,
            1,
        )
        .unwrap();
        assert_eq!(settings.timeout(), std::time::Duration::from_secs(10));
        assert_eq!(settings.read_retry_policy().max_attempts(), 1);

        let tuned = MidenRpcSettings::resolve(NetworkType::MidenLocal, None, 10_000, 2).unwrap();
        assert_eq!(tuned.timeout(), std::time::Duration::from_millis(10_000));
        assert_eq!(tuned.read_retry_policy().max_attempts(), 2);
    }

    #[test]
    fn rpc_settings_from_env_reject_malformed_numbers() {
        let _lock = ENV_LOCK.lock().unwrap();
        let var = MidenRpcSettings::TIMEOUT_ENV;
        unsafe { std::env::set_var(var, "not-a-number") };
        let error = MidenRpcSettings::from_env(NetworkType::MidenLocal).unwrap_err();
        unsafe { std::env::remove_var(var) };
        assert!(error.contains(var), "{error}");

        let attempts_var = MidenRpcSettings::MAX_ATTEMPTS_ENV;
        unsafe { std::env::set_var(attempts_var, "0") };
        let error = MidenRpcSettings::from_env(NetworkType::MidenLocal).unwrap_err();
        unsafe { std::env::remove_var(attempts_var) };
        assert!(error.contains(attempts_var), "{error}");
    }

    #[test]
    fn rpc_endpoint_validation_matches_the_fixture_corpus() {
        #[derive(serde::Deserialize)]
        struct Fixtures {
            endpoints: Vec<EndpointFixture>,
        }
        #[derive(serde::Deserialize)]
        struct EndpointFixture {
            input: String,
            valid: bool,
        }
        let fixtures: Fixtures = serde_json::from_str(include_str!(
            "../../../../fixtures/miden-multisig-client/rpc-policy-fixtures.json"
        ))
        .expect("fixtures must parse");
        for fixture in fixtures.endpoints {
            let result = MidenRpcSettings::resolve(
                NetworkType::MidenLocal,
                Some(crate::secret::CredentialUrl::new(fixture.input.clone())),
                30_000,
                1,
            );
            assert_eq!(result.is_ok(), fixture.valid, "input: {:?}", fixture.input);
        }
    }

    #[test]
    fn rpc_settings_from_env_map_miden_networks_to_the_miden_family() {
        let _lock = ENV_LOCK.lock().unwrap();
        for network in [
            NetworkType::MidenLocal,
            NetworkType::MidenTestnet,
            NetworkType::MidenDevnet,
        ] {
            let settings = super::RpcSettings::from_env(network).unwrap();
            let super::RpcSettings::Miden(miden) = settings;
            assert_eq!(miden.endpoint().expose_secret(), network.rpc_endpoint());
        }
    }
}
