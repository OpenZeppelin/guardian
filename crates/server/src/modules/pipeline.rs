//! Ordered evaluation of an account's modules at one hook.

use guardian_module_sdk::{Decision, Effect, HookRequest, is_valid_reason_code};

use super::catalog::{Catalog, ClientReason, HookName, OnError, Scope};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::registry::{CallError, Instance, Registry, isolated};
use super::request::ViewCache;
use super::store::{AccountParamsRow, ModuleStore, ParamsKind};
use crate::delta_summary::{FailedOpen, ModuleApproval};
use crate::error::GuardianError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    Global,
    Account,
}

impl Layer {
    pub fn as_str(self) -> &'static str {
        match self {
            Layer::Global => "global",
            Layer::Account => "account",
        }
    }
}

pub struct ModulePipeline {
    instances: Vec<Instance>,
    store: Arc<dyn ModuleStore>,
    cache: RwLock<Cache>,
    views: Arc<ViewCache>,
    pub catalog_hash: String,
}

/// The `module_instances` rows as of one params version.
const SNAPSHOT_ATTEMPTS: usize = 3;

pub(super) type Rows = Arc<HashMap<String, (serde_json::Value, bool)>>;

#[derive(Default)]
struct Cache {
    version: Option<u64>,
    rows: Rows,
}

/// The modules one request runs through, resolved once with a single store read.
pub struct Plan<'a> {
    hook: HookName,
    account_id: String,
    modules: Vec<(&'a Instance, serde_json::Value, Layer)>,
    catalog_hash: String,
    params_version: u64,
    views: Arc<ViewCache>,
}

enum Failure {
    Error(String),
    Timeout,
    InvalidDecision(String),
    /// An `effect` value this Guardian does not know: never read as an allow.
    UnknownEffect(i32),
    RequestUnavailable(String),
}

impl Failure {
    fn kind(&self) -> &'static str {
        match self {
            Failure::Error(_) => "error",
            Failure::Timeout => "timeout",
            Failure::InvalidDecision(_) | Failure::UnknownEffect(_) => "invalid_decision",
            Failure::RequestUnavailable(_) => "request_unavailable",
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Error(message) => write!(f, "{message}"),
            Failure::Timeout => write!(f, "timed out"),
            Failure::InvalidDecision(code) => write!(f, "invalid reason code {code:?}"),
            Failure::UnknownEffect(value) => write!(f, "unknown decision effect {value}"),
            Failure::RequestUnavailable(message) => write!(f, "request unavailable: {message}"),
        }
    }
}

impl ModulePipeline {
    pub async fn new(
        mut catalog: Catalog,
        registry: &Registry,
        store: Arc<dyn ModuleStore>,
        now: String,
    ) -> Result<Self, String> {
        let stored: HashMap<String, (serde_json::Value, bool)> = store
            .instances()
            .await?
            .into_iter()
            .map(|row| (row.module_id, (row.global_params, row.enabled)))
            .collect();
        let mut effective = HashMap::new();
        for config in &mut catalog.instances {
            let Some((params, enabled)) = stored.get(&config.id) else {
                continue;
            };
            if *enabled != config.catalog_enabled {
                tracing::warn!(module = %config.id, enabled, "Catalog `enabled` differs from the stored runtime flag; the stored flag applies");
            }
            config.enabled = *enabled;
            if !config.file_managed_params && *params != config.params {
                tracing::warn!(module = %config.id, "Catalog params differ from the operator-managed global params; the stored params apply");
                effective.insert(config.id.clone(), params.clone());
            }
        }
        let super::registry::Instantiated {
            instances,
            normalized,
        } = registry.instantiate(&catalog, &effective).await?;
        for config in catalog
            .instances
            .iter()
            .filter(|config| !stored.contains_key(&config.id))
        {
            let seed = if config.file_managed_params {
                serde_json::json!({})
            } else {
                normalized.get(&config.id).unwrap_or(&config.params).clone()
            };
            store
                .seed_instance(&config.id, config.enabled, &seed, &now)
                .await?;
        }
        for instance in instances
            .iter()
            .filter(|instance| !instance.config.file_managed_params)
        {
            let (Some(params), Some((read, _))) = (
                normalized.get(&instance.config.id),
                stored.get(&instance.config.id),
            ) else {
                continue;
            };
            let operator = format!(
                "module:{}",
                instance.build.as_deref().unwrap_or(&instance.config.kind)
            );
            let migrated = store
                .set_global_if(
                    &instance.config.id,
                    read,
                    params.clone(),
                    &super::store::Change {
                        operator: &operator,
                        reason: "params migrated by the module",
                        at: now.clone(),
                    },
                )
                .await?;
            if migrated {
                tracing::info!(module = %instance.config.id, %operator, "Module normalized its stored global params; stored the migrated form");
            } else {
                tracing::warn!(module = %instance.config.id, "Global params changed while the module migrated them; kept the newer value, migration retries next start");
            }
        }
        Ok(Self {
            instances,
            store,
            cache: RwLock::new(Cache::default()),
            views: Arc::default(),
            catalog_hash: catalog.hash,
        })
    }

    #[cfg(test)]
    pub fn from_instances(instances: Vec<Instance>) -> Self {
        Self::from_instances_with_store(
            instances,
            Arc::new(super::store::FsModuleStore::in_memory()),
        )
    }

    #[cfg(test)]
    pub fn from_instances_with_store(
        instances: Vec<Instance>,
        store: Arc<dyn ModuleStore>,
    ) -> Self {
        Self {
            instances,
            store,
            cache: RwLock::new(Cache::default()),
            views: Arc::default(),
            catalog_hash: String::new(),
        }
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn instance(&self, module_id: &str) -> Option<&Instance> {
        self.instances
            .iter()
            .find(|instance| instance.config.id == module_id)
    }

    pub fn store(&self) -> &dyn ModuleStore {
        self.store.as_ref()
    }

    fn store_error(error: String) -> GuardianError {
        GuardianError::StorageError(format!("module state unavailable: {error}"))
    }

    /// The stored `(global params, enabled)` rows, cached per replica and refreshed when the
    /// params version moves. One version read per call.
    pub(super) async fn rows(&self) -> Result<Rows, GuardianError> {
        Ok(self.versioned_rows().await?.1)
    }

    async fn versioned_rows(&self) -> Result<(u64, Rows), GuardianError> {
        let version = self.store.version().await.map_err(Self::store_error)?;
        if let Ok(cache) = self.cache.read()
            && cache.version == Some(version)
        {
            return Ok((version, cache.rows.clone()));
        }
        let rows: Rows = Arc::new(
            self.store
                .instances()
                .await
                .map_err(Self::store_error)?
                .into_iter()
                .map(|row| (row.module_id, (row.global_params, row.enabled)))
                .collect(),
        );
        if let Ok(mut cache) = self.cache.write() {
            *cache = Cache {
                version: Some(version),
                rows: rows.clone(),
            };
        }
        Ok((version, rows))
    }

    /// The global rows and one account's rows as of a single params version. Every module write
    /// moves the version in the same transaction, so an unchanged version across the reads
    /// means no write committed between them; otherwise the reads are retried. Without this a
    /// request could combine a new global rule with an old account layer, a combination no
    /// operator ever configured.
    pub(super) async fn snapshot(
        &self,
        account_id: &str,
    ) -> Result<(u64, Rows, Vec<AccountParamsRow>), GuardianError> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let (version, rows) = self.versioned_rows().await?;
            let account_rows = self
                .store
                .account_params(account_id)
                .await
                .map_err(Self::store_error)?;
            if self.store.version().await.map_err(Self::store_error)? == version {
                return Ok((version, rows, account_rows));
            }
        }
        Err(Self::store_error(
            "module state kept changing while it was read".into(),
        ))
    }

    pub(super) fn global_params_in(rows: &Rows, instance: &Instance) -> serde_json::Value {
        if instance.config.file_managed_params {
            return instance.config.params.clone();
        }
        rows.get(&instance.config.id).map_or_else(
            || instance.config.params.clone(),
            |(params, _)| params.clone(),
        )
    }

    pub(super) fn enabled_in(rows: &Rows, instance: &Instance) -> bool {
        rows.get(&instance.config.id)
            .map_or(instance.config.enabled, |(_, enabled)| *enabled)
    }

    /// The instance's global params: the stored ones, or the catalog's when they are
    /// file-managed or not seeded yet.
    pub async fn global_params(
        &self,
        instance: &Instance,
    ) -> Result<serde_json::Value, GuardianError> {
        Ok(Self::global_params_in(&self.rows().await?, instance))
    }

    /// Whether the instance is switched on: the stored runtime flag, or the startup flag for an
    /// instance without a row yet.
    pub async fn enabled(&self, instance: &Instance) -> Result<bool, GuardianError> {
        Ok(Self::enabled_in(&self.rows().await?, instance))
    }

    /// The modules that run for this account at this hook, or `None` when there are none, so a
    /// caller skips building the request.
    pub async fn plan(
        &self,
        hook: HookName,
        account_id: &str,
    ) -> Result<Option<Plan<'_>>, GuardianError> {
        let account_id = account_id.to_ascii_lowercase();
        let subscribed = |rows: &Rows, instance: &&Instance| {
            instance.config.hooks.contains(&hook) && Self::enabled_in(rows, instance)
        };
        let (_, rows) = self.versioned_rows().await?;
        if !self
            .instances
            .iter()
            .any(|instance| subscribed(&rows, &instance))
        {
            return Ok(None);
        }
        let (params_version, rows, account_rows) = self.snapshot(&account_id).await?;
        let active: Vec<&Instance> = self
            .instances
            .iter()
            .filter(|instance| subscribed(&rows, instance))
            .collect();
        if active.is_empty() {
            return Ok(None);
        }
        let mut modules = Vec::new();
        for instance in active
            .iter()
            .filter(|instance| instance.config.scope == Scope::All)
        {
            modules.push((
                *instance,
                Self::global_params_in(&rows, instance),
                Layer::Global,
            ));
            if let Some(layer) = account_rows
                .iter()
                .find(|row| row.kind == ParamsKind::Layer && row.module_id == instance.config.id)
            {
                modules.push((*instance, layer.params.clone(), Layer::Account));
            }
        }
        for row in account_rows
            .iter()
            .filter(|row| row.kind == ParamsKind::Assignment)
        {
            if let Some(instance) = active.iter().find(|instance| {
                instance.config.id == row.module_id && instance.config.scope != Scope::All
            }) {
                modules.push((*instance, row.params.clone(), Layer::Account));
            }
        }
        Ok((!modules.is_empty()).then_some(Plan {
            hook,
            account_id,
            modules,
            catalog_hash: self.catalog_hash.clone(),
            params_version,
            views: self.views.clone(),
        }))
    }

    /// The plan for a dry-run check: instances with `dry_run = false` are skipped, and
    /// `complete` is false when any was.
    pub async fn plan_dry_run(
        &self,
        hook: HookName,
        account_id: &str,
    ) -> Result<(Option<Plan<'_>>, bool), GuardianError> {
        let Some(mut plan) = self.plan(hook, account_id).await? else {
            return Ok((None, true));
        };
        let planned = plan.modules.len();
        plan.modules
            .retain(|(instance, _, _)| instance.config.dry_run);
        let complete = plan.modules.len() == planned;
        Ok(((!plan.modules.is_empty()).then_some(plan), complete))
    }

    pub async fn applies(&self, hook: HookName, account_id: &str) -> Result<bool, GuardianError> {
        Ok(self.plan(hook, account_id).await?.is_some())
    }

    /// Plans and runs the hook in one step.
    pub async fn run(
        &self,
        hook: HookName,
        account_id: &str,
        build: impl FnOnce() -> Result<HookRequest, String>,
    ) -> Result<(), GuardianError> {
        match self.plan(hook, account_id).await? {
            Some(plan) => plan.run(build).await.map(|_| ()),
            None => Ok(()),
        }
    }
}

impl Plan<'_> {
    /// The replica's account view cache, for building this plan's request.
    pub fn views(&self) -> Arc<ViewCache> {
        self.views.clone()
    }

    /// Runs the planned modules in order and returns the configuration they ran with. If
    /// `build` fails, every module counts as failed and its `on_error` applies.
    pub async fn run(
        self,
        build: impl FnOnce() -> Result<HookRequest, String>,
    ) -> Result<ModuleApproval, GuardianError> {
        let Plan {
            hook,
            account_id,
            modules,
            catalog_hash,
            params_version,
            views: _,
        } = self;
        let account_id = account_id.as_str();
        let mut approval = ModuleApproval {
            catalog_hash,
            params_version,
            failed_open: Vec::new(),
        };
        let base = match build() {
            Ok(base) => base,
            Err(error) => {
                for (instance, _, layer) in &modules {
                    let failure = Failure::RequestUnavailable(error.clone());
                    approval
                        .failed_open
                        .extend(fail(instance, *layer, account_id, &failure)?);
                }
                return Ok(approval);
            }
        };

        for (instance, params, layer) in modules {
            let mut request = base.clone();
            request.hook = hook.proto() as i32;
            request.instance_id = instance.config.id.clone();
            request.params = serde_json::to_vec(&params).unwrap_or_default();

            let module = instance.module.clone();
            let outcome = isolated(instance.config.timeout, async move {
                module.evaluate(&request).await
            })
            .await;
            let failure = match outcome.map(|decision| (effect(&decision), decision)) {
                Ok((Some(Effect::Allow), _)) => continue,
                Ok((Some(Effect::Deny), decision))
                    if is_valid_reason_code(&decision.reason_code) =>
                {
                    tracing::info!(
                        module = %instance.config.id,
                        account_id,
                        ?hook,
                        reason_code = %decision.reason_code,
                        layer = layer.as_str(),
                        message = %decision.message,
                        client_reason = ?instance.config.client_reason,
                        "Policy module denied the request"
                    );
                    return Err(match instance.config.client_reason {
                        ClientReason::Generic => GuardianError::ModuleDeniedGeneric,
                        ClientReason::Full => GuardianError::ModuleDenied {
                            module: instance.config.id.clone(),
                            reason_code: decision.reason_code,
                            layer: layer.as_str().to_string(),
                            client_message: client_message(&decision.client_message),
                        },
                    });
                }
                Ok((Some(_), decision)) => Failure::InvalidDecision(decision.reason_code),
                Ok((None, decision)) => Failure::UnknownEffect(decision.effect),
                Err(CallError::Module(error)) => Failure::Error(error.to_string()),
                Err(CallError::Panicked(message)) => {
                    Failure::Error(format!("module panicked: {message}"))
                }
                Err(CallError::Timeout) => Failure::Timeout,
            };
            approval
                .failed_open
                .extend(fail(instance, layer, account_id, &failure)?);
        }
        Ok(approval)
    }
}

const CLIENT_MESSAGE_MAX_CHARS: usize = 256;

/// A module's text for the requester as plain text: control characters dropped, whitespace
/// collapsed, at most 256 characters, and none when empty.
fn client_message(text: &str) -> Option<String> {
    let plain: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let capped: String = plain.chars().take(CLIENT_MESSAGE_MAX_CHARS).collect();
    (!capped.is_empty()).then_some(capped)
}

/// What a decision asks for. A module that predates `effect` leaves it unspecified and the
/// legacy `allow` decides; a value this Guardian does not know is `None`.
fn effect(decision: &Decision) -> Option<Effect> {
    match Effect::try_from(decision.effect) {
        Ok(Effect::Unspecified) if decision.allow => Some(Effect::Allow),
        Ok(Effect::Unspecified) => Some(Effect::Deny),
        Ok(effect) => Some(effect),
        Err(_) => None,
    }
}

/// Applies the instance's `on_error`: a refusal for `deny`, the failed-open record for `allow`.
fn fail(
    instance: &Instance,
    layer: Layer,
    account_id: &str,
    failure: &Failure,
) -> Result<Option<FailedOpen>, GuardianError> {
    match instance.config.on_error {
        OnError::Deny => {
            tracing::warn!(module = %instance.config.id, account_id, %failure, "Policy module failed; refusing");
            Err(GuardianError::ModuleUnavailable {
                module: instance.config.id.clone(),
            })
        }
        OnError::Allow => {
            tracing::warn!(module = %instance.config.id, account_id, %failure, "Policy module failed; continuing (fail-open)");
            Ok(Some(FailedOpen {
                module_id: instance.config.id.clone(),
                layer: layer.as_str().to_string(),
                failure: failure.kind().to_string(),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::modules::testing::{Behaviour, StubModule, instance, pipeline};
    use guardian_module_sdk::Decision;

    const HOOK: HookName = HookName::BeforeTransactionApproved;

    async fn run(pipeline: &ModulePipeline) -> Result<(), GuardianError> {
        pipeline
            .run(HOOK, "0xacc", || Ok(HookRequest::default()))
            .await
    }

    fn caps_catalog(params: &str) -> Catalog {
        Catalog::parse(
            &format!(
                r#"
[[module]]
id = "caps"
kind = "basic-policy"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = {params}
"#
            ),
            None,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn invalid_catalog_params_stop_startup_without_seeding_them() {
        let store: Arc<dyn ModuleStore> = Arc::new(super::super::store::FsModuleStore::in_memory());
        let registry = Registry::first_party();
        let now = "2026-10-15T00:00:00Z".to_string();
        let started = ModulePipeline::new(
            caps_catalog(r#"{ max_amount_per_txx = {} }"#),
            &registry,
            store.clone(),
            now.clone(),
        )
        .await;
        assert!(started.is_err());
        assert!(store.instances().await.unwrap().is_empty());

        let fixed = caps_catalog(r#"{ max_amount_per_tx = { "0xaa" = "5" } }"#);
        let started = ModulePipeline::new(fixed, &registry, store.clone(), now)
            .await
            .expect("the corrected catalog starts");
        assert!(
            started.instances()[0]
                .build
                .as_deref()
                .is_some_and(|build| build.starts_with("basic-policy"))
        );
        assert_eq!(store.instances().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stored_params_not_the_stale_catalog_seed_are_checked_at_startup() {
        let store: Arc<dyn ModuleStore> = Arc::new(super::super::store::FsModuleStore::in_memory());
        let registry = Registry::first_party();
        let now = "2026-10-15T00:00:00Z".to_string();
        store
            .seed_instance(
                "caps",
                true,
                &serde_json::json!({ "max_amount_per_tx": {} }),
                &now,
            )
            .await
            .unwrap();
        ModulePipeline::new(
            caps_catalog(r#"{ max_amount_per_txx = {} }"#),
            &registry,
            store.clone(),
            now.clone(),
        )
        .await
        .expect("the stored params apply, so the stale seed is not checked");

        store
            .set_global(
                "caps",
                serde_json::json!({ "unknown": true }),
                &super::super::store::Change {
                    operator: "0xop",
                    reason: "test",
                    at: now.clone(),
                },
            )
            .await
            .unwrap();
        let valid_seed = caps_catalog(r#"{ max_amount_per_tx = {} }"#);
        assert!(
            ModulePipeline::new(valid_seed, &registry, store, now)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fail_closed_timeout_refuses_as_unavailable() {
        let pipeline = pipeline(vec![instance(
            "slow",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Hang),
        )]);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleUnavailable { module }) if module == "slow"
        ));
    }

    #[tokio::test]
    async fn fail_open_failure_continues_to_the_next_module() {
        let denier = StubModule::denying("limit_exceeded");
        let pipeline = pipeline(vec![
            instance(
                "down",
                &[HOOK],
                OnError::Allow,
                StubModule::new(Behaviour::Fail),
            ),
            instance("caps", &[HOOK], OnError::Deny, denier.clone()),
        ]);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleDenied { module, reason_code, .. })
                if module == "caps" && reason_code == "limit_exceeded"
        ));
        assert_eq!(denier.calls(), 1);
    }

    #[tokio::test]
    async fn fail_open_timeout_alone_allows() {
        let pipeline = pipeline(vec![instance(
            "slow",
            &[HOOK],
            OnError::Allow,
            StubModule::new(Behaviour::Hang),
        )]);
        assert!(run(&pipeline).await.is_ok());
    }

    #[tokio::test]
    async fn the_trace_keeps_only_what_failed_open() {
        let pipeline = pipeline(vec![
            instance("caps", &[HOOK], OnError::Deny, StubModule::allowing()),
            instance(
                "risk",
                &[HOOK],
                OnError::Allow,
                StubModule::new(Behaviour::Hang),
            ),
        ]);
        let plan = || async { pipeline.plan(HOOK, "0xacc").await.unwrap().unwrap() };
        let approval = plan()
            .await
            .run(|| Ok(HookRequest::default()))
            .await
            .unwrap();
        assert_eq!(
            approval.failed_open,
            vec![FailedOpen {
                module_id: "risk".into(),
                layer: "global".into(),
                failure: "timeout".into(),
            }]
        );

        let unbuilt = plan().await.run(|| Err("no state".to_string())).await;
        assert!(unbuilt.is_err(), "caps fails closed without a request");
    }

    #[tokio::test]
    async fn a_panicking_module_is_a_module_failure() {
        let pipeline = pipeline(vec![instance(
            "buggy",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Panic),
        )]);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleUnavailable { module }) if module == "buggy"
        ));

        let pipeline = pipeline_with(OnError::Allow, Behaviour::Panic);
        let approval = pipeline
            .plan(HOOK, "0xacc")
            .await
            .unwrap()
            .unwrap()
            .run(|| Ok(HookRequest::default()))
            .await
            .unwrap();
        assert_eq!(approval.failed_open[0].failure, "error");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_timeout_holds_when_a_module_blocks() {
        let pipeline = pipeline_with(OnError::Deny, Behaviour::Block(Duration::from_secs(2)));
        let started = std::time::Instant::now();
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleUnavailable { .. })
        ));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "answered at the timeout, not when the module returned"
        );
    }

    fn pipeline_with(on_error: OnError, behaviour: Behaviour) -> Arc<ModulePipeline> {
        pipeline(vec![instance(
            "stub",
            &[HOOK],
            on_error,
            StubModule::new(behaviour),
        )])
    }

    fn decision(allow: bool, reason_code: &str, effect: i32) -> Behaviour {
        Behaviour::Decide(Decision {
            allow,
            reason_code: reason_code.into(),
            effect,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn the_effect_decides_and_an_unknown_one_is_a_failure() {
        let denied = |behaviour| async {
            matches!(
                run(&pipeline_with(OnError::Deny, behaviour)).await,
                Err(GuardianError::ModuleDenied { .. })
            )
        };
        assert!(
            denied(decision(false, "legacy_deny", Effect::Unspecified as i32)).await,
            "a module without effect: allow = false still denies"
        );
        assert!(
            denied(decision(true, "explicit_deny", Effect::Deny as i32)).await,
            "effect wins over allow"
        );
        assert!(
            run(&pipeline_with(
                OnError::Deny,
                decision(false, "", Effect::Allow as i32)
            ))
            .await
            .is_ok()
        );
        assert!(
            matches!(
                run(&pipeline_with(OnError::Deny, decision(true, "", 99))).await,
                Err(GuardianError::ModuleUnavailable { .. })
            ),
            "an effect this Guardian does not know is never read as an allow"
        );
    }

    #[tokio::test]
    async fn startup_stores_params_the_module_migrated() {
        use crate::modules::testing::{FixedFactory, ProtocolStub};
        let catalog = || {
            Catalog::parse(
                r#"
[[module]]
id = "migrating"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = { old_name = 5 }
"#,
                None,
            )
            .unwrap()
        };
        let mut stub = ProtocolStub::new();
        stub.normalized = Some(serde_json::json!({ "new_name": 5 }));
        let mut registry = Registry::first_party();
        registry
            .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
            .unwrap();
        let store: Arc<dyn ModuleStore> = Arc::new(super::super::store::FsModuleStore::in_memory());
        let now = "2026-10-16T00:00:00Z".to_string();

        ModulePipeline::new(catalog(), &registry, store.clone(), now.clone())
            .await
            .unwrap();
        let seeded = store.instances().await.unwrap();
        assert_eq!(
            seeded[0].global_params,
            serde_json::json!({ "new_name": 5 }),
            "a new instance is seeded in the migrated shape"
        );

        store
            .set_global(
                "migrating",
                serde_json::json!({ "old_name": 7 }),
                &super::super::store::Change {
                    operator: "0xop",
                    reason: "old shape",
                    at: now.clone(),
                },
            )
            .await
            .unwrap();
        ModulePipeline::new(catalog(), &registry, store.clone(), now)
            .await
            .unwrap();
        assert_eq!(
            store.instances().await.unwrap()[0].global_params,
            serde_json::json!({ "new_name": 5 })
        );
        let last = store.history("migrating").await.unwrap().pop().unwrap();
        assert_eq!(last.operator, "module:protocol-stub 1");
        assert_eq!(last.reason, "params migrated by the module");
        assert_eq!(last.previous, serde_json::json!({ "old_name": 7 }));
    }

    #[tokio::test]
    async fn a_generic_instance_denies_without_details() {
        let deny = Behaviour::Decide(
            Decision::deny("recipient_sanctioned", "on list X")
                .with_client_message("contact support"),
        );
        let mut instance = instance("screening", &[HOOK], OnError::Deny, StubModule::new(deny));
        instance.config.client_reason = ClientReason::Generic;
        assert!(matches!(
            run(&pipeline(vec![instance])).await,
            Err(GuardianError::ModuleDeniedGeneric)
        ));
    }

    #[tokio::test]
    async fn a_full_denial_forwards_a_cleaned_client_message() {
        let text = format!(
            "Limit reached.\n\tAsk your\u{7} treasury admin. {}",
            "x".repeat(400)
        );
        let deny = Behaviour::Decide(
            Decision::deny("amount_cap_exceeded", "internal").with_client_message(text),
        );
        match run(&pipeline_with(OnError::Deny, deny)).await {
            Err(GuardianError::ModuleDenied {
                client_message: Some(message),
                ..
            }) => {
                assert!(message.starts_with("Limit reached. Ask your treasury admin."));
                assert_eq!(message.chars().count(), 256);
                assert!(!message.chars().any(char::is_control));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(client_message("  \n "), None);
    }

    #[tokio::test]
    async fn a_request_never_mixes_a_global_rule_and_an_account_layer_from_different_versions() {
        use super::super::store::Change;
        use crate::modules::testing::{RacingStore, RacingWrite};
        let store = Arc::new(RacingStore::new());
        let change = Change {
            operator: "0xop",
            reason: "setup",
            at: "2026-10-16T00:00:00Z".into(),
        };
        store
            .inner
            .seed_instance(
                "caps",
                true,
                &serde_json::json!({ "cap": 1000 }),
                "2026-10-16T00:00:00Z",
            )
            .await
            .unwrap();
        store
            .inner
            .set_layer(
                "0xacc",
                "caps",
                Some(serde_json::json!({ "cap": 100 })),
                &change,
            )
            .await
            .unwrap();
        *store.before_account_params.lock().unwrap() = vec![
            RacingWrite::Global("caps", serde_json::json!({ "cap": 100 })),
            RacingWrite::Layer("0xacc", "caps", None),
        ];
        let pipeline = ModulePipeline::from_instances_with_store(
            vec![instance(
                "caps",
                &[HOOK],
                OnError::Deny,
                StubModule::allowing(),
            )],
            store,
        );

        let plan = pipeline.plan(HOOK, "0xacc").await.unwrap().unwrap();
        let params: Vec<_> = plan
            .modules
            .iter()
            .map(|(_, params, _)| params.clone())
            .collect();
        assert_eq!(
            params,
            vec![serde_json::json!({ "cap": 100 })],
            "the version moved during the read, so the plan was rebuilt from one version"
        );
    }

    /// An operator lowers the stored params while the module is migrating them at startup;
    /// the migration must not replace the operator's newer value with its stale result.
    #[tokio::test]
    async fn a_startup_migration_never_overwrites_a_newer_operator_write() {
        use crate::modules::testing::{FixedFactory, ProtocolStub, RacingStore, RacingWrite};
        let catalog = Catalog::parse(
            r#"
[[module]]
id = "migrating"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
params = { old_name = 1000 }
"#,
            None,
        )
        .unwrap();
        let mut stub = ProtocolStub::new();
        stub.normalized = Some(serde_json::json!({ "new_name": 1000 }));
        let mut registry = Registry::first_party();
        registry
            .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
            .unwrap();
        let store = Arc::new(RacingStore::new());
        store
            .inner
            .seed_instance(
                "migrating",
                true,
                &serde_json::json!({ "old_name": 1000 }),
                "2026-10-16T00:00:00Z",
            )
            .await
            .unwrap();
        *store.before_set_global_if.lock().unwrap() = vec![RacingWrite::Global(
            "migrating",
            serde_json::json!({ "new_name": 100 }),
        )];

        ModulePipeline::new(
            catalog,
            &registry,
            store.clone(),
            "2026-10-16T00:00:02Z".into(),
        )
        .await
        .unwrap();
        assert_eq!(
            store.instances().await.unwrap()[0].global_params,
            serde_json::json!({ "new_name": 100 }),
            "the operator's cap of 100 stays"
        );
        let last = store.history("migrating").await.unwrap().pop().unwrap();
        assert_eq!(
            last.operator, "0xracing-operator",
            "no migration entry after it"
        );
    }

    #[tokio::test]
    async fn a_deny_stops_evaluation() {
        let later = StubModule::allowing();
        let pipeline = pipeline(vec![
            instance("first", &[HOOK], OnError::Deny, StubModule::denying("no")),
            instance("second", &[HOOK], OnError::Deny, later.clone()),
        ]);
        assert!(run(&pipeline).await.is_err());
        assert_eq!(later.calls(), 0);
    }

    #[tokio::test]
    async fn malformed_reason_code_is_a_failure() {
        let pipeline = pipeline(vec![instance(
            "bad",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Decide(Decision::deny("Not Valid!", ""))),
        )]);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleUnavailable { .. })
        ));
    }

    #[tokio::test]
    async fn request_build_failure_applies_each_failure_mode() {
        let closed = pipeline(vec![instance(
            "caps",
            &[HOOK],
            OnError::Deny,
            StubModule::allowing(),
        )]);
        let result = closed
            .run(HOOK, "0xacc", || Err("view failed".into()))
            .await;
        assert!(matches!(
            result,
            Err(GuardianError::ModuleUnavailable { .. })
        ));

        let open = pipeline(vec![instance(
            "caps",
            &[HOOK],
            OnError::Allow,
            StubModule::allowing(),
        )]);
        assert!(
            open.run(HOOK, "0xacc", || Err("view failed".into()))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn modules_not_subscribed_to_the_hook_are_not_called() {
        let other = StubModule::denying("no");
        let pipeline = pipeline(vec![instance(
            "registration-only",
            &[HookName::BeforeAccountRegistered],
            OnError::Deny,
            other.clone(),
        )]);
        assert!(!pipeline.applies(HOOK, "0xacc").await.unwrap());
        assert!(run(&pipeline).await.is_ok());
        assert_eq!(other.calls(), 0);
    }

    mod layers {
        use super::*;
        use crate::modules::store::Change;
        use crate::modules::testing::instance_with_params;
        use guardian_module_sdk::basic_policy::BasicPolicy;
        use guardian_module_sdk::proto::hook_request;
        use guardian_module_sdk::{OutputNote, TransactionSubject, TxProjection};
        use serde_json::json;

        fn pay(recipient: &str) -> HookRequest {
            HookRequest {
                subject: Some(hook_request::Subject::Transaction(TransactionSubject {
                    projection: Some(TxProjection {
                        output_notes: vec![OutputNote {
                            note_id: "0xn".into(),
                            p2id_target: Some(recipient.into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                })),
                ..Default::default()
            }
        }

        fn blocklist() -> ModulePipeline {
            ModulePipeline::from_instances(vec![instance_with_params(
                "blocklist",
                &[HOOK],
                std::sync::Arc::new(BasicPolicy),
                json!({ "recipient_denylist": ["0xaaa"] }),
            )])
        }

        fn change() -> Change<'static> {
            Change {
                operator: "0xop",
                reason: "test",
                at: "t".into(),
            }
        }

        async fn layer_of(
            pipeline: &ModulePipeline,
            account: &str,
            recipient: &str,
        ) -> Option<String> {
            match pipeline.run(HOOK, account, || Ok(pay(recipient))).await {
                Ok(()) => None,
                Err(GuardianError::ModuleDenied { layer, .. }) => Some(layer),
                Err(other) => panic!("unexpected {other:?}"),
            }
        }

        #[tokio::test]
        async fn account_layer_extends_the_global_rule() {
            let pipeline = blocklist();
            pipeline
                .store()
                .set_layer(
                    "0xacc",
                    "blocklist",
                    Some(json!({ "recipient_denylist": ["0xccc"] })),
                    &change(),
                )
                .await
                .unwrap();

            assert_eq!(
                layer_of(&pipeline, "0xacc", "0xaaa").await.as_deref(),
                Some("global")
            );
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0xccc").await.as_deref(),
                Some("account")
            );
            assert_eq!(layer_of(&pipeline, "0xacc", "0xbbb").await, None);
            assert_eq!(layer_of(&pipeline, "0xother", "0xccc").await, None);
            assert_eq!(
                layer_of(&pipeline, "0xother", "0xaaa").await.as_deref(),
                Some("global")
            );
        }

        #[tokio::test]
        async fn an_account_layer_cannot_loosen_the_global_rule() {
            let pipeline = blocklist();
            pipeline
                .store()
                .set_layer("0xacc", "blocklist", Some(json!({})), &change())
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0xaaa").await.as_deref(),
                Some("global")
            );
        }

        #[tokio::test]
        async fn global_params_changed_at_runtime_apply_to_the_next_evaluation() {
            let pipeline = blocklist();
            pipeline
                .store()
                .seed_instance(
                    "blocklist",
                    true,
                    &json!({ "recipient_denylist": ["0xaaa"] }),
                    "t0",
                )
                .await
                .unwrap();
            pipeline
                .store()
                .set_global(
                    "blocklist",
                    json!({ "recipient_denylist": ["0xaaa", "0xddd"] }),
                    &change(),
                )
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xany", "0xddd").await.as_deref(),
                Some("global")
            );
        }

        #[tokio::test]
        async fn a_module_disabled_at_runtime_is_skipped_until_enabled_again() {
            let pipeline = blocklist();
            pipeline
                .store()
                .seed_instance(
                    "blocklist",
                    true,
                    &json!({ "recipient_denylist": ["0xaaa"] }),
                    "t0",
                )
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0xaaa").await.as_deref(),
                Some("global")
            );
            pipeline
                .store()
                .set_enabled("blocklist", false, &change())
                .await
                .unwrap();
            assert_eq!(layer_of(&pipeline, "0xacc", "0xaaa").await, None);
            assert!(!pipeline.applies(HOOK, "0xacc").await.unwrap());
            pipeline
                .store()
                .set_enabled("blocklist", true, &change())
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0xaaa").await.as_deref(),
                Some("global")
            );
        }
    }
}
