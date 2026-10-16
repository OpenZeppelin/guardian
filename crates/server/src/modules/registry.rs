//! Module kinds and instance creation.

use std::collections::HashMap;
use std::sync::Arc;

use guardian_module_sdk::basic_policy::{self, BasicPolicy};
use guardian_module_sdk::{
    Module, ModuleError, ModuleFactory, ModuleInstanceConfig, PROTOCOL_VERSION,
};

use super::catalog::{Catalog, InstanceConfig};
use super::grpc_kind::GrpcFactory;

pub struct Instance {
    pub config: InstanceConfig,
    pub module: Arc<dyn Module>,
    /// `name version` from `describe` at startup; `None` when the checks failed.
    pub build: Option<String>,
}

struct BasicPolicyFactory;

impl ModuleFactory for BasicPolicyFactory {
    fn create(&self, _instance: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError> {
        Ok(Arc::new(BasicPolicy))
    }
}

pub struct Registry {
    factories: HashMap<String, Arc<dyn ModuleFactory>>,
}

impl Registry {
    pub fn first_party() -> Self {
        let mut registry = Self {
            factories: HashMap::new(),
        };
        registry
            .factories
            .insert(basic_policy::KIND.into(), Arc::new(BasicPolicyFactory));
        registry
            .factories
            .insert("grpc".into(), Arc::new(GrpcFactory));
        registry
    }

    /// Adds a kind the catalog can name. A kind is registered once; the first-party names
    /// (`basic-policy`, `grpc`) cannot be replaced.
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
    /// created, is unsupported, unreachable or misconfigured stops startup; a disabled one is
    /// logged and kept disabled.
    pub async fn instantiate(
        &self,
        catalog: &Catalog,
        effective: &HashMap<String, serde_json::Value>,
    ) -> Result<Vec<Instance>, String> {
        let mut instances = Vec::with_capacity(catalog.instances.len());
        for config in &catalog.instances {
            let mut config = config.clone();
            let params = effective.get(&config.id).unwrap_or(&config.params);
            let ready = match self.create(&config) {
                Ok(module) => check(&config, &module, params)
                    .await
                    .map(|info| (module.clone(), info))
                    .map_err(|error| (error, Some(module))),
                Err(error) => Err((error, None)),
            };
            let (module, build) = match ready {
                Ok((module, info)) => (module, Some(format!("{} {}", info.name, info.version))),
                Err((error, _)) if config.enabled => {
                    return Err(format!("module {}: {error}", config.id));
                }
                Err((error, module)) => {
                    tracing::warn!(module = %config.id, %error, "Disabled module failed its startup checks");
                    config.enabled = false;
                    (module.unwrap_or_else(|| Arc::new(Unavailable(error))), None)
                }
            };
            instances.push(Instance {
                config,
                module,
                build,
            });
        }
        Ok(instances)
    }

    fn create(&self, config: &InstanceConfig) -> Result<Arc<dyn Module>, String> {
        let factory = self
            .factories
            .get(&config.kind)
            .ok_or_else(|| format!("unknown kind {}", config.kind))?;
        let instance = ModuleInstanceConfig {
            id: config.id.clone(),
            kind: config.kind.clone(),
            endpoint: config.endpoint.clone(),
            auth_token: config.auth_token.clone(),
            tls: config.tls.clone(),
            timeout: config.timeout,
            params: config.params.clone(),
        };
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| factory.create(&instance)))
            .map_err(|_| "module factory panicked".to_string())?
            .map_err(|e| e.to_string())
    }
}

/// Stands in for a disabled instance that could not be created; never evaluated while disabled.
struct Unavailable(String);

#[async_trait::async_trait]
impl Module for Unavailable {
    async fn describe(&self) -> Result<guardian_module_sdk::ModuleInfo, ModuleError> {
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
/// instead of awaiting. A call that times out is aborted at its next await point.
pub async fn isolated<T: Send + 'static>(
    timeout: std::time::Duration,
    call: impl std::future::Future<Output = Result<T, ModuleError>> + Send + 'static,
) -> Result<T, CallError> {
    let mut task = tokio::spawn(call);
    match tokio::time::timeout(timeout, &mut task).await {
        Ok(Ok(result)) => result.map_err(CallError::Module),
        Ok(Err(error)) => Err(CallError::Panicked(match error.try_into_panic() {
            Ok(payload) => payload
                .downcast_ref::<&str>()
                .map(|message| message.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".into()),
            Err(error) => error.to_string(),
        })),
        Err(_) => {
            task.abort();
            Err(CallError::Timeout)
        }
    }
}

/// Startup checks: protocol version, hook support and the params that will apply. Also run
/// before enabling.
pub async fn check(
    config: &InstanceConfig,
    module: &Arc<dyn Module>,
    params: &serde_json::Value,
) -> Result<guardian_module_sdk::ModuleInfo, String> {
    let describing = module.clone();
    let info = isolated(config.timeout, async move { describing.describe().await })
        .await
        .map_err(|e| format!("describe {e}"))?;
    if info.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "unsupported protocol version {}",
            info.protocol_version
        ));
    }
    for hook in &config.hooks {
        if !info.hooks.contains(&(hook.proto() as i32)) {
            return Err(format!("module does not support hook {hook:?}"));
        }
    }

    validate_params(module, params, config.timeout).await?;
    Ok(info)
}

/// Asks the module whether `params` are valid; used at startup and for every params write.
pub async fn validate_params(
    module: &Arc<dyn Module>,
    params: &serde_json::Value,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(params).map_err(|e| e.to_string())?;
    let validating = module.clone();
    let verdict = isolated(
        timeout,
        async move { validating.validate_params(&bytes).await },
    )
    .await
    .map_err(|e| format!("validate_params {e}"))?;
    if verdict.ok {
        Ok(())
    } else {
        Err(format!("invalid params: {}", verdict.message))
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
                )
                .await
                .expect("a disabled instance does not block startup");
            assert_eq!(instances.len(), 1);
            assert!(!instances[0].config.enabled);
        }
    }

    #[tokio::test]
    async fn enabled_instances_that_cannot_be_created_stop_startup() {
        for entry in [UNKNOWN_KIND, BAD_ENDPOINT] {
            let result = Registry::first_party()
                .instantiate(&catalog(&entry.replace("ENABLED", "true")), &HashMap::new())
                .await;
            assert!(result.is_err());
        }
    }

    #[tokio::test]
    async fn the_guide_catalogs_load() {
        let dir = std::env::temp_dir().join(format!("guide_{:016x}", rand::random::<u64>()));
        std::fs::create_dir_all(&dir).unwrap();
        for token in ["caps.token", "note-policy.token"] {
            std::fs::write(dir.join(token), "t").unwrap();
        }
        for text in [
            include_str!("../../../../docs/guides/policy-modules/remote.toml"),
            include_str!("../../../../docs/guides/policy-modules/custom.toml"),
            include_str!("../../../../docs/guides/policy-modules/custom-in-process.toml"),
        ] {
            Catalog::parse(text, Some(&dir)).expect("guide catalog parses");
        }
        std::fs::remove_dir_all(dir).ok();
        let built_in = include_str!("../../../../docs/guides/policy-modules/built-in.toml");
        let instances = Registry::first_party()
            .instantiate(&catalog(built_in), &HashMap::new())
            .await
            .expect("the built-in guide catalog passes the startup checks");
        assert!(instances.iter().all(|instance| instance.config.enabled));
    }

    struct StubFactory;

    impl ModuleFactory for StubFactory {
        fn create(&self, _: &ModuleInstanceConfig) -> Result<Arc<dyn Module>, ModuleError> {
            Ok(Arc::new(BasicPolicy))
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
            .instantiate(&catalog(CUSTOM), &HashMap::new())
            .await
            .unwrap();
        assert_eq!(instances[0].config.kind, "house-policy");
        assert!(instances[0].config.enabled);
        assert!(
            Registry::first_party()
                .instantiate(&catalog(CUSTOM), &HashMap::new())
                .await
                .is_err(),
            "unregistered, the same catalog stops startup"
        );
    }

    #[test]
    fn a_kind_name_is_taken_once() {
        let mut registry = Registry::first_party();
        for taken in [basic_policy::KIND, "grpc"] {
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
            .instantiate(&catalog(CUSTOM), &HashMap::new())
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
}
