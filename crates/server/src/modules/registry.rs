//! Module kinds and instance creation.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use guardian_module_sdk::policies::{
    self, CodePolicy, RecipientPolicy, SignerPolicy, SpendingLimit,
};
use guardian_module_sdk::{
    DescribeRequest, Module, ModuleError, ModuleFactory, ModuleInfo, ModuleInstanceConfig,
    PROTOCOL_VERSION, capability,
};

use super::catalog::{Catalog, InstanceConfig};
use super::grpc_kind::GrpcFactory;

#[derive(Clone)]
pub struct Instance {
    pub config: InstanceConfig,
    pub module: Arc<dyn Module>,
    /// `name version` from `describe` at startup; `None` when the checks failed.
    pub build: Option<String>,
}

/// A first-party policy: stateless, so every instance of the kind shares one module; each
/// call carries its instance's params.
struct FirstParty(Arc<dyn Module>);

impl ModuleFactory for FirstParty {
    fn create(&self, _instance: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError> {
        Ok(self.0.clone())
    }
}

/// The created instances, and the params each module normalized at its startup check (only
/// the instances whose normalized params differ from the ones checked).
pub struct Instantiated {
    pub instances: Vec<Instance>,
    pub normalized: HashMap<String, serde_json::Value>,
    /// Enabled instances whose module did not answer its startup checks and that start
    /// unavailable: their checks run again on each health probe.
    pub unreachable: HashSet<String>,
}

pub struct Registry {
    factories: HashMap<String, Arc<dyn ModuleFactory>>,
}

impl Registry {
    pub fn first_party() -> Self {
        let mut registry = Self {
            factories: HashMap::new(),
        };
        let policies: [(&str, Arc<dyn Module>); 4] = [
            (policies::spending_limit::KIND, Arc::new(SpendingLimit)),
            (policies::recipient_policy::KIND, Arc::new(RecipientPolicy)),
            (policies::signer_policy::KIND, Arc::new(SignerPolicy)),
            (policies::code_policy::KIND, Arc::new(CodePolicy)),
        ];
        for (kind, module) in policies {
            registry
                .factories
                .insert(kind.into(), Arc::new(FirstParty(module)));
        }
        registry
            .factories
            .insert("grpc".into(), Arc::new(GrpcFactory));
        registry
    }

    /// Adds a kind the catalog can name. A kind is registered once; the first-party names
    /// (the four policies and `grpc`) cannot be replaced.
    pub fn register(
        &mut self,
        kind: impl Into<String>,
        factory: Arc<dyn ModuleFactory>,
    ) -> Result<(), String> {
        let kind = kind.into();
        if self.factories.contains_key(&kind) {
            return Err(format!("module kind {kind} is already registered"));
        }
        self.factories.insert(kind, factory);
        Ok(())
    }

    /// Creates every instance and checks it against the params that will apply (`effective`,
    /// keyed by instance id; the catalog's when absent). An enabled instance that cannot be
    /// created, is unsupported or misconfigured stops startup; a disabled one is logged and
    /// kept disabled.
    ///
    /// An enabled instance whose module does not answer starts unavailable when it is `known`
    /// (it ran on an earlier start), so a module's outage does not keep the Guardian from
    /// restarting: its accounts follow `on_error` until a health probe passes the checks. One
    /// that is new to this Guardian must answer, so a wrong endpoint in a new catalog entry is
    /// found when it is deployed.
    pub async fn instantiate(
        &self,
        catalog: &Catalog,
        effective: &HashMap<String, serde_json::Value>,
        known: &HashSet<String>,
    ) -> Result<Instantiated, String> {
        let mut instances = Vec::with_capacity(catalog.instances.len());
        let mut normalized = HashMap::new();
        let mut unreachable = HashSet::new();
        for config in &catalog.instances {
            let mut config = config.clone();
            let params = effective.get(&config.id).unwrap_or(&config.params);
            let ready = match self.create(&config) {
                Ok(module) => check(&config, &module, params)
                    .await
                    .map(|checked| (module.clone(), checked))
                    .map_err(|error| (error, Some(module))),
                Err(error) => Err((Unhealthy::Incompatible(error), None)),
            };
            let (module, build) = match ready {
                Ok((module, checked)) => {
                    if checked.params != *params {
                        normalized.insert(config.id.clone(), checked.params);
                    }
                    (
                        module,
                        Some(format!("{} {}", checked.info.name, checked.info.version)),
                    )
                }
                Err((Unhealthy::Unreachable(error), Some(module)))
                    if config.enabled && known.contains(&config.id) =>
                {
                    tracing::warn!(module = %config.id, %error, "Enabled module did not answer its startup checks; it starts unavailable and its accounts follow on_error until a health probe passes");
                    unreachable.insert(config.id.clone());
                    (module, None)
                }
                Err((error, _)) if config.enabled => {
                    return Err(format!("module {}: {error}", config.id));
                }
                Err((error, module)) => {
                    tracing::warn!(module = %config.id, %error, "Disabled module failed its startup checks");
                    config.enabled = false;
                    (
                        module.unwrap_or_else(|| Arc::new(Unavailable(error.to_string()))),
                        None,
                    )
                }
            };
            instances.push(Instance {
                config,
                module,
                build,
            });
        }
        Ok(Instantiated {
            instances,
            normalized,
            unreachable,
        })
    }

    fn create(&self, config: &InstanceConfig) -> Result<Arc<dyn Module>, String> {
        let factory = self
            .factories
            .get(&config.kind)
            .ok_or_else(|| format!("unknown kind {}", config.kind))?;
        let credentials = config.credentials.clone()?;
        let instance = ModuleInstanceConfig::new(
            config.id.clone(),
            config.kind.clone(),
            config.timeout,
            config.params.clone(),
        )
        .with_endpoint(config.endpoint.clone())
        .with_auth_token(credentials.auth_token)
        .with_tls(credentials.tls)
        .with_secrets(credentials.params);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory.create(&instance)))
            .map_err(|_| "module factory panicked".to_string())?
            .map_err(|e| e.to_string())
    }
}

/// Stands in for a disabled instance that could not be created; never evaluated while disabled.
struct Unavailable(String);

#[async_trait::async_trait]
impl Module for Unavailable {
    async fn describe(
        &self,
        _guardian: &guardian_module_sdk::DescribeRequest,
    ) -> Result<guardian_module_sdk::ModuleInfo, ModuleError> {
        Err(ModuleError::Unavailable(self.0.clone()))
    }

    async fn validate_params(
        &self,
        _params: &[u8],
    ) -> Result<guardian_module_sdk::ParamsVerdict, ModuleError> {
        Err(ModuleError::Unavailable(self.0.clone()))
    }

    async fn evaluate(
        &self,
        _request: &guardian_module_sdk::HookRequest,
    ) -> Result<guardian_module_sdk::Decision, ModuleError> {
        Err(ModuleError::Unavailable(self.0.clone()))
    }
}

/// Why a module call produced no result.
#[derive(Debug)]
pub enum CallError {
    Timeout,
    /// The panic message, when the payload was a string.
    Panicked(String),
    Module(ModuleError),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::Timeout => write!(f, "timed out"),
            CallError::Panicked(message) => write!(f, "panicked: {message}"),
            CallError::Module(error) => write!(f, "{error}"),
        }
    }
}

/// Runs one module call on its own task, so a panic in the module becomes an error instead of
/// unwinding through the request, and the timeout holds even when an in-process module blocks
/// instead of awaiting. A call that times out, or whose caller goes away, is aborted at its next
/// await point.
pub async fn isolated<T: Send + 'static>(
    timeout: std::time::Duration,
    call: impl std::future::Future<Output = Result<T, ModuleError>> + Send + 'static,
) -> Result<T, CallError> {
    struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);
    impl<T> Drop for AbortOnDrop<T> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let mut task = AbortOnDrop(tokio::spawn(call));
    match tokio::time::timeout(timeout, &mut task.0).await {
        Ok(Ok(result)) => result.map_err(CallError::Module),
        Ok(Err(error)) => Err(CallError::Panicked(match error.try_into_panic() {
            Ok(payload) => payload
                .downcast_ref::<&str>()
                .map(|message| message.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into()),
            Err(error) => error.to_string(),
        })),
        Err(_) => Err(CallError::Timeout),
    }
}

/// The oldest module protocol version this Guardian accepts. It equals `PROTOCOL_VERSION`
/// until a second major version exists; from then on the previous major stays accepted for a
/// deprecation window (RFC 0002 section 3.5).
pub const MIN_PROTOCOL_VERSION: u32 = PROTOCOL_VERSION;

/// Optional protocol features this Guardian provides.
pub const CAPABILITIES: &[&str] = &[capability::PARAMS_NORMALIZATION];

/// What the Guardian tells a module in `Describe`.
pub fn describe_request() -> DescribeRequest {
    DescribeRequest {
        guardian_protocol_version: PROTOCOL_VERSION,
        guardian_min_protocol_version: MIN_PROTOCOL_VERSION,
        guardian_capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
    }
}

/// A module that passed its startup checks, and the params to store for it (the module's
/// normalized form when it returned one).
pub struct Checked {
    pub info: ModuleInfo,
    pub params: serde_json::Value,
}

/// Startup checks: protocol version, required capabilities, hook support and the params that
/// will apply. Also run before enabling.
pub async fn check(
    config: &InstanceConfig,
    module: &Arc<dyn Module>,
    params: &serde_json::Value,
) -> Result<Checked, Unhealthy> {
    let info = describe_healthy(config, module).await?;
    let params = validate_params(module, params, config.timeout).await?;
    Ok(Checked { info, params })
}

/// Why a module failed its checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unhealthy {
    /// The module did not answer: it may be down.
    Unreachable(String),
    /// The module answered, but with something this Guardian cannot work with (it refused the
    /// call, or speaks another protocol version, requires a capability this Guardian lacks or
    /// no longer supports a configured hook), so its decisions cannot be trusted either.
    Incompatible(String),
    /// The module refuses the params that would apply to it.
    InvalidParams(String),
}

impl Unhealthy {
    /// A call that produced no result: no answer in time, or the module reporting itself
    /// unavailable, is unreachable; anything else is an answer this Guardian cannot use.
    fn call(name: &str, error: CallError) -> Self {
        let detail = format!("{name} {error}");
        match error {
            CallError::Timeout | CallError::Module(ModuleError::Unavailable(_)) => {
                Unhealthy::Unreachable(detail)
            }
            CallError::Module(_) | CallError::Panicked(_) => Unhealthy::Incompatible(detail),
        }
    }
}

impl std::fmt::Display for Unhealthy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unhealthy::Unreachable(detail)
            | Unhealthy::Incompatible(detail)
            | Unhealthy::InvalidParams(detail) => {
                write!(f, "{detail}")
            }
        }
    }
}

impl From<Unhealthy> for String {
    fn from(unhealthy: Unhealthy) -> Self {
        unhealthy.to_string()
    }
}

/// `Describe` under the instance's timeout, refused when the module speaks a protocol version
/// outside the supported range, requires a capability this Guardian lacks, or no longer supports
/// a configured hook. The startup checks and the periodic health probe share it.
pub async fn describe_healthy(
    config: &InstanceConfig,
    module: &Arc<dyn Module>,
) -> Result<ModuleInfo, Unhealthy> {
    let describing = module.clone();
    let info = isolated(config.timeout, async move {
        describing.describe(&describe_request()).await
    })
    .await
    .map_err(|e| Unhealthy::call("describe", e))?;
    if !(MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION).contains(&info.protocol_version) {
        return Err(Unhealthy::Incompatible(format!(
            "unsupported protocol version {}; this Guardian accepts {MIN_PROTOCOL_VERSION} to {PROTOCOL_VERSION}",
            info.protocol_version
        )));
    }
    if info.protocol_version < PROTOCOL_VERSION {
        tracing::warn!(module = %config.id, protocol_version = info.protocol_version, current = PROTOCOL_VERSION, "Module speaks a deprecated protocol version");
    }
    if let Some(missing) = info
        .required_capabilities
        .iter()
        .find(|required| !CAPABILITIES.contains(&required.as_str()))
    {
        return Err(Unhealthy::Incompatible(format!(
            "module requires capability {missing}, which this Guardian does not provide"
        )));
    }
    for hook in &config.hooks {
        if !info.hooks.contains(&(hook.proto() as i32)) {
            return Err(Unhealthy::Incompatible(format!(
                "module does not support hook {hook:?}"
            )));
        }
    }
    Ok(info)
}

/// Asks the module whether `params` are valid; used at startup and for every params write.
/// Returns the params to store: the module's normalized form when it sent one, otherwise
/// `params` unchanged.
pub async fn validate_params(
    module: &Arc<dyn Module>,
    params: &serde_json::Value,
    timeout: std::time::Duration,
) -> Result<serde_json::Value, Unhealthy> {
    let bytes = serde_json::to_vec(params).map_err(|e| Unhealthy::InvalidParams(e.to_string()))?;
    let validating = module.clone();
    let verdict = isolated(
        timeout,
        async move { validating.validate_params(&bytes).await },
    )
    .await
    .map_err(|e| Unhealthy::call("validate_params", e))?;
    if !verdict.ok {
        return Err(Unhealthy::InvalidParams(format!(
            "invalid params: {}",
            verdict.message
        )));
    }
    if verdict.normalized_params.is_empty() {
        return Ok(params.clone());
    }
    match serde_json::from_slice::<serde_json::Value>(&verdict.normalized_params) {
        Ok(normalized) if normalized.is_object() => Ok(normalized),
        _ => Err(Unhealthy::Incompatible(
            "normalized params are not a JSON object".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(entry: &str) -> Catalog {
        Catalog::parse(entry, None).unwrap()
    }

    const UNKNOWN_KIND: &str = r#"
[[module]]
id = "staged"
kind = "vendor-not-built-in"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 20
enabled = ENABLED
"#;

    const BAD_ENDPOINT: &str = r#"
[[module]]
id = "staged"
kind = "grpc"
endpoint = "grpc://not a valid uri"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 20
enabled = ENABLED
"#;

    #[tokio::test]
    async fn disabled_instances_that_cannot_be_created_stay_disabled() {
        for entry in [UNKNOWN_KIND, BAD_ENDPOINT] {
            let instances = Registry::first_party()
                .instantiate(
                    &catalog(&entry.replace("ENABLED", "false")),
                    &HashMap::new(),
                    &HashSet::new(),
                )
                .await
                .expect("a disabled instance does not block startup")
                .instances;
            assert_eq!(instances.len(), 1);
            assert!(!instances[0].config.enabled);
        }
    }

    #[tokio::test]
    async fn enabled_instances_that_cannot_be_created_stop_startup() {
        for entry in [UNKNOWN_KIND, BAD_ENDPOINT] {
            let result = Registry::first_party()
                .instantiate(
                    &catalog(&entry.replace("ENABLED", "true")),
                    &HashMap::new(),
                    &HashSet::new(),
                )
                .await;
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn the_guide_catalogs_load() {
        for text in [
            include_str!("../../../../docs/guides/policy-modules/remote.toml"),
            include_str!("../../../../docs/guides/policy-modules/custom.toml"),
            include_str!("../../../../docs/guides/policy-modules/custom-in-process.toml"),
            include_str!("../../../../docs/guides/policy-modules/compliance.toml"),
        ] {
            Catalog::parse(text, None).expect("guide catalog parses");
        }
        let built_in = include_str!("../../../../docs/guides/policy-modules/built-in.toml");
        let unedited = Registry::first_party()
            .instantiate(&catalog(built_in), &HashMap::new(), &HashSet::new())
            .await
            .err()
            .expect("the guide catalog does not start until its placeholder is replaced");
        assert!(
            unedited.contains("neither an account id in 0x hex form nor a bech32 account address"),
            "{unedited}"
        );
        let edited = built_in.replace(
            "0xREPLACE_WITH_FAUCET_ID",
            "0x0a1b2c3d4e5f60718293a4b5c6d7e8",
        );
        let started = Registry::first_party()
            .instantiate(&catalog(&edited), &HashMap::new(), &HashSet::new())
            .await
            .expect("the built-in guide catalog passes the startup checks");
        assert!(
            started
                .instances
                .iter()
                .all(|instance| instance.config.enabled)
        );
        assert!(
            started.normalized.is_empty(),
            "hex params are stored as written"
        );

        let by_address = built_in.replace(
            "0xREPLACE_WITH_FAUCET_ID",
            "mdev1aq0u96z73ckz2y2kr4xp57950qrn4xjc",
        );
        let started = Registry::first_party()
            .instantiate(&catalog(&by_address), &HashMap::new(), &HashSet::new())
            .await
            .expect("a faucet named by its bech32 address passes the startup checks");
        assert_eq!(
            started.normalized.get("caps"),
            Some(&serde_json::json!({
                "max_per_transaction": { "0x1fc2e85e8e2c2511561d4c1a78b478": "5" }
            })),
            "the hex id is what gets stored"
        );
    }

    struct StubFactory;

    impl ModuleFactory for StubFactory {
        fn create(&self, _: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError> {
            Ok(Arc::new(RecipientPolicy))
        }
    }

    struct PanickingFactory;

    impl ModuleFactory for PanickingFactory {
        fn create(&self, _: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError> {
            panic!("factory bug")
        }
    }

    const CUSTOM: &str = r#"
[[module]]
id = "house-rules"
kind = "house-policy"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
"#;

    #[tokio::test]
    async fn a_registered_kind_loads_from_the_catalog() {
        let mut registry = Registry::first_party();
        registry
            .register("house-policy", Arc::new(StubFactory))
            .unwrap();
        let instances = registry
            .instantiate(&catalog(CUSTOM), &HashMap::new(), &HashSet::new())
            .await
            .unwrap()
            .instances;
        assert_eq!(instances[0].config.kind, "house-policy");
        assert!(instances[0].config.enabled);
        assert!(
            Registry::first_party()
                .instantiate(&catalog(CUSTOM), &HashMap::new(), &HashSet::new())
                .await
                .is_err(),
            "unregistered, the same catalog stops startup"
        );
    }

    #[test]
    fn a_kind_name_is_taken_once() {
        let mut registry = Registry::first_party();
        for taken in policies::KINDS.into_iter().chain(["grpc"]) {
            assert!(registry.register(taken, Arc::new(StubFactory)).is_err());
        }
        registry
            .register("house-policy", Arc::new(StubFactory))
            .unwrap();
        assert!(
            registry
                .register("house-policy", Arc::new(StubFactory))
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_panicking_factory_fails_startup_cleanly() {
        let mut registry = Registry::first_party();
        registry
            .register("house-policy", Arc::new(PanickingFactory))
            .unwrap();
        let error = registry
            .instantiate(&catalog(CUSTOM), &HashMap::new(), &HashSet::new())
            .await
            .err()
            .unwrap();
        assert!(error.contains("factory panicked"), "{error}");
    }

    #[tokio::test]
    async fn a_panic_keeps_its_message() {
        let timeout = std::time::Duration::from_secs(1);
        let literal = isolated::<()>(timeout, async { panic!("literal message") }).await;
        assert!(
            matches!(literal, Err(CallError::Panicked(message)) if message == "literal message")
        );
        let id = 7;
        let formatted = isolated::<()>(timeout, async move { panic!("instance {id} broke") }).await;
        assert!(
            matches!(formatted, Err(CallError::Panicked(message)) if message == "instance 7 broke")
        );
    }

    const PROTOCOL_CATALOG: &str = r#"
[[module]]
id = "negotiated"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = { old_name = 5 }
"#;

    async fn load(stub: crate::modules::testing::ProtocolStub) -> Result<Instantiated, String> {
        let mut registry = Registry::first_party();
        registry
            .register(
                "protocol-stub",
                Arc::new(crate::modules::testing::FixedFactory(Arc::new(stub))),
            )
            .unwrap();
        registry
            .instantiate(&catalog(PROTOCOL_CATALOG), &HashMap::new(), &HashSet::new())
            .await
    }

    #[tokio::test]
    async fn describe_tells_the_module_what_this_guardian_speaks() {
        let stub = Arc::new(crate::modules::testing::ProtocolStub::new());
        let mut registry = Registry::first_party();
        registry
            .register(
                "protocol-stub",
                Arc::new(crate::modules::testing::FixedFactory(stub.clone())),
            )
            .unwrap();
        registry
            .instantiate(&catalog(PROTOCOL_CATALOG), &HashMap::new(), &HashSet::new())
            .await
            .unwrap();
        let sent = stub.described_with.lock().unwrap().clone().unwrap();
        assert_eq!(sent.guardian_protocol_version, PROTOCOL_VERSION);
        assert_eq!(sent.guardian_min_protocol_version, MIN_PROTOCOL_VERSION);
        assert!(
            sent.guardian_capabilities
                .contains(&capability::PARAMS_NORMALIZATION.to_string())
        );
    }

    #[tokio::test]
    async fn a_protocol_version_outside_the_supported_range_is_refused() {
        for version in [MIN_PROTOCOL_VERSION - 1, PROTOCOL_VERSION + 1] {
            let mut stub = crate::modules::testing::ProtocolStub::new();
            stub.protocol_version = version;
            let error = load(stub).await.err().unwrap();
            assert!(error.contains("unsupported protocol version"), "{error}");
        }
    }

    #[tokio::test]
    async fn a_capability_this_guardian_lacks_is_refused() {
        let mut stub = crate::modules::testing::ProtocolStub::new();
        stub.required_capabilities = vec!["pending.v2".into()];
        let error = load(stub).await.err().unwrap();
        assert!(error.contains("requires capability pending.v2"), "{error}");

        let mut stub = crate::modules::testing::ProtocolStub::new();
        stub.required_capabilities = vec![capability::PARAMS_NORMALIZATION.into()];
        assert!(load(stub).await.is_ok());
    }

    #[tokio::test]
    async fn normalized_params_come_back_from_the_startup_check() {
        let mut stub = crate::modules::testing::ProtocolStub::new();
        stub.normalized = Some(serde_json::json!({ "new_name": 5 }));
        let loaded = load(stub).await.unwrap();
        assert_eq!(
            loaded.normalized.get("negotiated"),
            Some(&serde_json::json!({ "new_name": 5 }))
        );
        assert!(
            load(crate::modules::testing::ProtocolStub::new())
                .await
                .unwrap()
                .normalized
                .is_empty(),
            "a module that returns nothing changes nothing"
        );
    }
}
