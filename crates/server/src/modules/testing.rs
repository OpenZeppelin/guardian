use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use guardian_module_sdk::{
    Decision, HookRequest, Module, ModuleError, ModuleInfo, PROTOCOL_VERSION, ParamsVerdict,
};

use super::catalog::{HookName, InstanceConfig, OnError, Scope};
use super::pipeline::ModulePipeline;
use super::registry::Instance;

#[derive(Clone)]
pub enum Behaviour {
    Decide(Decision),
    Fail,
    Hang,
}

pub struct StubModule {
    behaviour: Behaviour,
    pub calls: AtomicUsize,
    pub last_request: std::sync::Mutex<Option<HookRequest>>,
}

impl StubModule {
    pub fn new(behaviour: Behaviour) -> Arc<Self> {
        Arc::new(Self {
            behaviour,
            calls: AtomicUsize::new(0),
            last_request: std::sync::Mutex::new(None),
        })
    }

    pub fn allowing() -> Arc<Self> {
        Self::new(Behaviour::Decide(Decision::allow()))
    }

    pub fn denying(reason_code: &str) -> Arc<Self> {
        Self::new(Behaviour::Decide(Decision::deny(reason_code, "stub")))
    }

    pub fn last_request(&self) -> Option<HookRequest> {
        self.last_request.lock().unwrap().clone()
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Module for StubModule {
    async fn describe(&self) -> Result<ModuleInfo, ModuleError> {
        Ok(ModuleInfo {
            name: "stub".into(),
            version: "0".into(),
            protocol_version: PROTOCOL_VERSION,
            hooks: vec![],
        })
    }

    async fn validate_params(&self, _params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(ParamsVerdict::ok())
    }

    async fn evaluate(&self, request: &HookRequest) -> Result<Decision, ModuleError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_request.lock().unwrap() = Some(request.clone());
        match &self.behaviour {
            Behaviour::Decide(decision) => Ok(decision.clone()),
            Behaviour::Fail => Err(ModuleError::Unavailable("stub down".into())),
            Behaviour::Hang => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(Decision::allow())
            }
        }
    }
}

pub fn instance(
    id: &str,
    hooks: &[HookName],
    on_error: OnError,
    module: Arc<dyn Module>,
) -> Instance {
    Instance {
        config: InstanceConfig {
            id: id.into(),
            kind: "stub".into(),
            endpoint: None,
            scope: Scope::All,
            hooks: hooks.to_vec(),
            timeout: Duration::from_millis(50),
            on_error,
            enabled: true,
            catalog_enabled: true,
            operator_can_disable: true,
            auth_token: None,
            tls: None,
            params: serde_json::json!({}),
            file_managed_params: false,
        },
        module,
        build: Some("stub 1".into()),
    }
}

pub fn instance_with_params(
    id: &str,
    hooks: &[HookName],
    module: Arc<dyn Module>,
    params: serde_json::Value,
) -> Instance {
    let mut instance = instance(id, hooks, OnError::Deny, module);
    instance.config.params = params;
    instance
}

pub fn pipeline(instances: Vec<Instance>) -> Arc<ModulePipeline> {
    Arc::new(ModulePipeline::from_instances(instances))
}
