use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use guardian_module_sdk::{
    Decision, DescribeRequest, HookRequest, Module, ModuleError, ModuleInfo, PROTOCOL_VERSION,
    ParamsVerdict,
};

use super::catalog::{HookName, InstanceConfig, OnError, Scope};
use super::pipeline::ModulePipeline;
use super::registry::Instance;

#[derive(Clone)]
pub enum Behaviour {
    Decide(Decision),
    Fail,
    Hang,
    Panic,
    /// Blocks its thread instead of awaiting, like a module doing synchronous work.
    Block(Duration),
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
    async fn describe(&self, _guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        Ok(ModuleInfo {
            name: "stub".into(),
            version: "0".into(),
            protocol_version: PROTOCOL_VERSION,
            hooks: vec![],
            ..Default::default()
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
            Behaviour::Panic => panic!("stub module panicked"),
            Behaviour::Block(duration) => {
                std::thread::sleep(*duration);
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
            disclosure: Default::default(),
            description: None,
            client_reason: Default::default(),
            dry_run: true,
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

/// A module whose `describe` and `validate_params` answers are configurable, for the protocol
/// negotiation and params migration tests. It records what the Guardian sent in `Describe`.
pub struct ProtocolStub {
    pub protocol_version: u32,
    pub required_capabilities: Vec<String>,
    pub normalized: Option<serde_json::Value>,
    pub described_with: std::sync::Mutex<Option<DescribeRequest>>,
}

impl Default for ProtocolStub {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolStub {
    pub fn new() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            required_capabilities: Vec::new(),
            normalized: None,
            described_with: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl Module for ProtocolStub {
    async fn describe(&self, guardian: &DescribeRequest) -> Result<ModuleInfo, ModuleError> {
        *self.described_with.lock().unwrap() = Some(guardian.clone());
        Ok(ModuleInfo {
            name: "protocol-stub".into(),
            version: "1".into(),
            protocol_version: self.protocol_version,
            hooks: vec![guardian_module_sdk::Hook::BeforeTransactionApproved as i32],
            required_capabilities: self.required_capabilities.clone(),
        })
    }

    async fn validate_params(&self, _params: &[u8]) -> Result<ParamsVerdict, ModuleError> {
        Ok(match &self.normalized {
            Some(params) => ParamsVerdict::normalized(params),
            None => ParamsVerdict::ok(),
        })
    }

    async fn evaluate(&self, _request: &HookRequest) -> Result<Decision, ModuleError> {
        Ok(Decision::allow())
    }
}

/// Hands out one prepared module for every instance of its kind.
pub struct FixedFactory(pub Arc<dyn Module>);

impl guardian_module_sdk::ModuleFactory for FixedFactory {
    fn create(
        &self,
        _instance: &guardian_module_sdk::ModuleInstanceConfig,
    ) -> Result<Arc<dyn Module>, ModuleError> {
        Ok(self.0.clone())
    }
}
