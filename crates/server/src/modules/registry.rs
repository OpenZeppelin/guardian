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
        registry.register(basic_policy::KIND, Arc::new(BasicPolicyFactory));
        registry.register("grpc", Arc::new(GrpcFactory));
        registry
    }

    pub fn register(&mut self, kind: impl Into<String>, factory: Arc<dyn ModuleFactory>) {
        self.factories.insert(kind.into(), factory);
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
                Ok(module) => check(&config, module.as_ref(), params)
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
        factory
            .create(&ModuleInstanceConfig {
                id: config.id.clone(),
                kind: config.kind.clone(),
                endpoint: config.endpoint.clone(),
                auth_token: config.auth_token.clone(),
                tls: config.tls.clone(),
                timeout: config.timeout,
                params: config.params.clone(),
            })
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

/// Startup checks: protocol version, hook support and the params that will apply. Also run
/// before enabling.
pub async fn check(
    config: &InstanceConfig,
    module: &dyn Module,
    params: &serde_json::Value,
) -> Result<guardian_module_sdk::ModuleInfo, String> {
    let info = tokio::time::timeout(config.timeout, module.describe())
        .await
        .map_err(|_| "describe timed out".to_string())?
        .map_err(|e| e.to_string())?;
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
    module: &dyn Module,
    params: &serde_json::Value,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let bytes = serde_json::to_vec(params).map_err(|e| e.to_string())?;
    let verdict = tokio::time::timeout(timeout, module.validate_params(&bytes))
        .await
        .map_err(|_| "validate_params timed out".to_string())?
        .map_err(|e| e.to_string())?;
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
}
