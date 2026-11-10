//! Ordered evaluation of an account's modules at one hook.

use guardian_module_sdk::{Decision, Effect, HookRequest, is_valid_reason_code};

use super::catalog::{Catalog, ClientReason, HookName, OnError, Scope};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use super::breaker::{Breakers, Caller, CircuitState};
use super::limits::Limits;
use super::registry::{CallError, Instance, Registry, Unhealthy, isolated};
use super::request::ViewCache;
use super::store::{AccountParamsRow, Assigned, ModuleStore, ParamsKind};
use crate::delta_summary::FailedOpen;
use crate::error::GuardianError;

/// The configuration a pipeline run evaluated with, and the failures `on_error = allow` let
/// through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub catalog_hash: String,
    pub params_version: u64,
    pub failed_open: Vec<FailedOpen>,
}

impl Evaluation {
    /// Records the failures `on_error = allow` let through at a hook that signs nothing, so they
    /// are kept somewhere other than the log. A transaction's are stored with its signed delta
    /// instead. `subject` adds what the hook judged, a proposal id for one.
    pub fn audit_failed_open(
        &self,
        auditor: &dyn crate::audit::Auditor,
        hook: HookName,
        account_id: &str,
        submitter: &str,
        subject: serde_json::Map<String, serde_json::Value>,
    ) {
        if self.failed_open.is_empty() {
            return;
        }
        let mut payload = serde_json::json!({
            "hook": hook.as_str(),
            "submitter": submitter,
            "catalog_hash": self.catalog_hash,
            "params_version": self.params_version,
            "failed_open": self.failed_open,
        });
        if let Some(fields) = payload.as_object_mut() {
            fields.extend(subject);
        }
        auditor.record(crate::audit::AuditEvent {
            operator_identity: crate::services::release_on_switch::SYSTEM_OPERATOR_IDENTITY
                .to_string(),
            action_kind: crate::audit::kinds::MODULES_FAILED_OPEN,
            target_account_id: Some(account_id.to_string()),
            payload,
            outcome: crate::audit::AuditOutcome::Success,
            error_code: None,
            client_ip: None,
        });
    }
}

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
    limits: Limits,
    breakers: Arc<Breakers>,
    pub catalog_hash: String,
    /// The network this Guardian serves, as modules receive it in `network_id`.
    network_id: &'static str,
    /// Instances whose stored seed differs from the catalog's params, as of startup.
    seed_drift: HashSet<String>,
    /// Accounts over the budget and orphaned assignment rows, as of the last report.
    assignment_report: std::sync::Mutex<(usize, usize)>,
    /// Instances that started without passing their startup checks because their module did
    /// not answer. Each health probe runs the checks again until they pass.
    unchecked: std::sync::Mutex<HashSet<String>>,
}

pub(super) type Rows = Arc<HashMap<String, (serde_json::Value, bool)>>;

/// How often the probe loop recounts the over-budget accounts and orphaned rows.
const ASSIGNMENT_REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// An account whose stored modules could take longer than the evaluation budget.
struct OverBudget {
    account_id: String,
    hook: HookName,
    worst_case: std::time::Duration,
}

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
    network_id: &'static str,
    catalog_hash: String,
    params_version: u64,
    views: Arc<ViewCache>,
    limits: Limits,
    breakers: Arc<Breakers>,
    /// A policy check: every request says so, and nothing is stored whatever the decision.
    dry_run: bool,
}

enum Failure {
    Error(String),
    /// The module refused this request as one it cannot judge: its failure for this request,
    /// which says nothing about whether the module is up.
    Rejected(String),
    Timeout,
    InvalidDecision(String),
    /// No `effect`, or a value this Guardian does not know: never read as an allow.
    UnknownEffect(i32),
    RequestUnavailable(String),
    /// The encoded request exceeds the size cap; it is not sent.
    RequestTooLarge(usize),
    /// The instance's breaker is open; the module is not called.
    CircuitOpen,
}

impl Failure {
    fn kind(&self) -> &'static str {
        match self {
            Failure::Error(_) | Failure::Rejected(_) => "error",
            Failure::Timeout => "timeout",
            Failure::InvalidDecision(_) | Failure::UnknownEffect(_) => "invalid_decision",
            Failure::RequestUnavailable(_) | Failure::RequestTooLarge(_) => "request_unavailable",
            Failure::CircuitOpen => "circuit_open",
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Error(message) => write!(f, "{message}"),
            Failure::Rejected(message) => write!(f, "module rejected the request: {message}"),
            Failure::Timeout => write!(f, "timed out"),
            Failure::InvalidDecision(code) => write!(f, "invalid reason code {code:?}"),
            Failure::UnknownEffect(value) => write!(f, "unknown decision effect {value}"),
            Failure::RequestUnavailable(message) => write!(f, "request unavailable: {message}"),
            Failure::RequestTooLarge(bytes) => {
                write!(f, "request of {bytes} bytes over the size cap")
            }
            Failure::CircuitOpen => write!(f, "circuit open"),
        }
    }
}

/// The rules every stored params value follows, applied to what a module returns as normalized
/// params: within the size cap and free of secret references.
fn storable(limits: &Limits, params: &serde_json::Value) -> Result<(), String> {
    limits.check_params_size(params)?;
    if super::catalog::contains_secret_reference(params) {
        return Err("normalized params contain a secret reference".into());
    }
    Ok(())
}

impl ModulePipeline {
    /// [`storable`] under this pipeline's limits.
    pub fn storable_normalization(&self, params: &serde_json::Value) -> Result<(), String> {
        storable(&self.limits, params)
    }

    pub async fn new(
        network_id: &'static str,
        mut catalog: Catalog,
        registry: &Registry,
        store: Arc<dyn ModuleStore>,
        now: String,
        limits: Limits,
    ) -> Result<Self, String> {
        for config in &catalog.instances {
            limits
                .check_params_size(&config.params)
                .map_err(|e| format!("module {}: catalog {e}", config.id))?;
        }
        let rows = store.instances().await?;
        let seeded: HashMap<String, serde_json::Value> = rows
            .iter()
            .map(|row| (row.module_id.clone(), row.seeded_params.clone()))
            .collect();
        let stored: HashMap<String, (serde_json::Value, bool)> = rows
            .into_iter()
            .map(|row| (row.module_id, (row.global_params, row.enabled)))
            .collect();
        let known: HashSet<String> = stored.keys().cloned().collect();
        let mut effective = HashMap::new();
        let mut seed_drift = HashSet::new();
        let mut enabled_by_catalog = Vec::new();
        for config in &mut catalog.instances {
            let Some((params, enabled)) = stored.get(&config.id) else {
                continue;
            };
            if !config.file_managed_params
                && seeded
                    .get(&config.id)
                    .is_some_and(|seeded| *seeded != config.params)
            {
                seed_drift.insert(config.id.clone());
            }
            // The stored flag is the switch, with one exception: an instance the catalog
            // declares enabled and not operator-disableable cannot be left off by a disable
            // that was allowed under an earlier catalog. Switching it on only tightens policy.
            let catalog_requires_it =
                !*enabled && config.catalog_enabled && !config.operator_can_disable;
            if catalog_requires_it {
                tracing::warn!(module = %config.id, "Stored flag disables an instance the catalog declares enabled and not operator-disableable; enabling it");
                enabled_by_catalog.push(config.id.clone());
            } else if *enabled != config.catalog_enabled {
                tracing::warn!(module = %config.id, enabled, "Catalog `enabled` differs from the stored runtime flag; the stored flag applies");
            }
            config.enabled = *enabled || catalog_requires_it;
            if !config.file_managed_params && *params != config.params {
                tracing::warn!(module = %config.id, "Catalog params differ from the operator-managed global params; the stored params apply");
                effective.insert(config.id.clone(), params.clone());
            }
        }
        let super::registry::Instantiated {
            instances,
            normalized,
            unreachable,
        } = registry.instantiate(&catalog, &effective, &known).await?;
        for module_id in enabled_by_catalog {
            store
                .set_enabled(
                    &module_id,
                    true,
                    &super::store::Change {
                        operator: "catalog",
                        reason: "the catalog declares the instance enabled and not operator-disableable",
                        at: now.clone(),
                    },
                )
                .await?;
        }
        for config in catalog
            .instances
            .iter()
            .filter(|config| !stored.contains_key(&config.id))
        {
            let seed = if config.file_managed_params {
                serde_json::json!({})
            } else {
                normalized
                    .get(&config.id)
                    .filter(|params| storable(&limits, params).is_ok())
                    .unwrap_or(&config.params)
                    .clone()
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
            if let Err(error) = storable(&limits, params) {
                tracing::warn!(module = %instance.config.id, %error, "Module's normalized params break the params rules; kept the stored params");
                continue;
            }
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
            if migrated.is_some() {
                tracing::info!(module = %instance.config.id, %operator, "Module normalized its stored global params; stored the migrated form");
            } else {
                tracing::warn!(module = %instance.config.id, "Global params changed while the module migrated them; kept the newer value, migration retries next start");
            }
        }
        let breakers = Arc::new(Breakers::new(limits.breaker_failures));
        for module_id in &unreachable {
            breakers.hold_open(module_id);
        }
        Ok(Self {
            instances,
            store,
            cache: RwLock::new(Cache::default()),
            views: Arc::default(),
            breakers,
            unchecked: std::sync::Mutex::new(unreachable),
            limits,
            catalog_hash: catalog.hash,
            network_id,
            seed_drift,
            assignment_report: std::sync::Mutex::new((0, 0)),
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
            limits: Limits::default(),
            breakers: Arc::new(Breakers::new(Limits::default().breaker_failures)),
            catalog_hash: String::new(),
            network_id: "local",
            seed_drift: HashSet::new(),
            assignment_report: std::sync::Mutex::new((0, 0)),
            unchecked: std::sync::Mutex::default(),
        }
    }

    #[cfg(test)]
    pub fn with_network(mut self, network_id: &'static str) -> Self {
        self.network_id = network_id;
        self
    }

    #[cfg(test)]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.breakers = Arc::new(Breakers::new(limits.breaker_failures));
        self.limits = limits;
        self
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn breakers(&self) -> &Breakers {
        &self.breakers
    }

    /// The hook with the largest worst-case module time for an account whose rows name
    /// `modules`, and that time: every `scope = all` instance, once more for an account layer on
    /// it, plus every listed instance. Disabled instances count, since enabling needs no further
    /// check; a row naming an instance the catalog no longer declares costs nothing.
    fn worst_case(&self, modules: &[(&str, ParamsKind)]) -> (HookName, std::time::Duration) {
        self.costs(modules)
            .into_iter()
            .max_by_key(|(_, total)| *total)
            .unwrap_or((
                HookName::BeforeTransactionApproved,
                std::time::Duration::ZERO,
            ))
    }

    /// The summed timeouts of the module calls an account naming `modules` makes at each gate
    /// hook.
    fn costs(&self, modules: &[(&str, ParamsKind)]) -> Vec<(HookName, std::time::Duration)> {
        let calls = |id: &str, kind: ParamsKind| {
            modules
                .iter()
                .filter(|(module_id, named)| *named == kind && *module_id == id)
                .count() as u32
        };
        HookName::GATES
            .into_iter()
            .map(|hook| {
                let total = self
                    .instances
                    .iter()
                    .filter(|instance| instance.config.hooks.contains(&hook))
                    .map(|instance| {
                        let id = instance.config.id.as_str();
                        let count = match instance.config.scope {
                            Scope::All => 1 + calls(id, ParamsKind::Layer),
                            Scope::Default | Scope::Assigned => calls(id, ParamsKind::Assignment),
                        };
                        instance.config.timeout * count
                    })
                    .sum();
                (hook, total)
            })
            .collect()
    }

    /// Refuses a write that would take the account's modules over the budget at a hook where
    /// they are not already at least that far over. A write that keeps or lowers the cost passes,
    /// so an account a deploy pushed over the budget can still be brought back under it.
    pub fn check_budget(
        &self,
        current: &[AccountParamsRow],
        proposed: &[AccountParamsRow],
    ) -> Result<(), GuardianError> {
        let named = |rows: &[AccountParamsRow]| -> Vec<(HookName, std::time::Duration)> {
            let modules: Vec<(&str, ParamsKind)> = rows
                .iter()
                .map(|row| (row.module_id.as_str(), row.kind))
                .collect();
            self.costs(&modules)
        };
        for ((hook, total), (_, now)) in named(proposed).into_iter().zip(named(current)) {
            if total > self.limits.budget && total > now {
                return Err(GuardianError::InvalidInput(format!(
                    "the account's modules at {} would take up to {} ms, over the evaluation budget of {} ms",
                    hook.as_str(),
                    total.as_millis(),
                    self.limits.budget.as_millis()
                )));
            }
        }
        Ok(())
    }

    /// The accounts whose stored modules exceed the budget, each with its worst hook and that
    /// hook's time, and the count of stored rows naming an instance the catalog no longer
    /// declares. One scan of which module each account row names, without params.
    async fn scan_assignments(&self) -> Result<(Vec<OverBudget>, usize), String> {
        let rows = self.store.account_modules().await?;
        let orphaned = rows
            .iter()
            .filter(|row| self.instance(&row.module_id).is_none())
            .count();
        let over = rows
            .chunk_by(|a, b| a.account_id == b.account_id)
            .filter_map(|account| {
                let modules: Vec<(&str, ParamsKind)> = account
                    .iter()
                    .map(|row| (row.module_id.as_str(), row.kind))
                    .collect();
                let (hook, worst_case) = self.worst_case(&modules);
                (worst_case > self.limits.budget).then(|| OverBudget {
                    account_id: account[0].account_id.clone(),
                    hook,
                    worst_case,
                })
            })
            .collect();
        Ok((over, orphaned))
    }

    /// The over-budget account count and the orphaned row count as last counted.
    #[cfg(test)]
    pub fn assignment_report(&self) -> (usize, usize) {
        self.assignment_report
            .lock()
            .map(|report| *report)
            .unwrap_or_default()
    }

    fn record_assignment_report(&self, over_budget: usize, orphaned: usize) {
        if let Ok(mut report) = self.assignment_report.lock() {
            *report = (over_budget, orphaned);
        }
    }

    /// Logs every account whose stored modules exceed the budget, without refusing to start: a
    /// deploy that raised a timeout must not become an outage. Returns the count of those
    /// accounts and of stored rows naming an instance the catalog no longer declares.
    pub async fn report_stored_assignments(&self) -> Result<(usize, usize), String> {
        let (over, orphaned) = self.scan_assignments().await?;
        for account in &over {
            tracing::warn!(
                account_id = %account.account_id,
                hook = account.hook.as_str(),
                worst_case_ms = account.worst_case.as_millis() as u64,
                budget_ms = self.limits.budget.as_millis() as u64,
                "Account's policy modules exceed the evaluation budget; the runtime budget applies"
            );
        }
        self.record_assignment_report(over.len(), orphaned);
        Ok((over.len(), orphaned))
    }

    /// Recounts the over-budget accounts and orphaned rows for the gauges, so they follow what
    /// operators change after startup: an alarm clears once the assignment is fixed.
    pub async fn refresh_assignment_report(&self) {
        match self.scan_assignments().await {
            Ok((over, orphaned)) => self.record_assignment_report(over.len(), orphaned),
            Err(error) => {
                tracing::warn!(%error, "Stored module assignments not recounted; the gauges keep their last values");
            }
        }
    }

    /// Sets the state gauges from the stored rows and this replica's breakers. Run on every probe,
    /// so they are set once a metrics recorder exists and follow changes other replicas make.
    fn publish(&self, rows: &Rows) {
        super::metrics::catalog(&self.catalog_hash);
        let (over_budget, orphaned) = self
            .assignment_report
            .lock()
            .map(|report| *report)
            .unwrap_or_default();
        super::metrics::stored_assignments(over_budget, orphaned);
        for instance in &self.instances {
            let id = &instance.config.id;
            let enabled = Self::enabled_in(rows, instance);
            super::metrics::enabled(id, enabled);
            super::metrics::config_drift(
                id,
                enabled != instance.config.catalog_enabled || self.seed_drift.contains(id),
            );
            super::metrics::circuit_open(id, self.breakers.state(id) == CircuitState::Open);
        }
    }

    /// One health probe of every enabled instance: `Describe` under its timeout, with the startup
    /// checks. A healthy module half-opens an open breaker so the next real call is sent. One
    /// that does not answer counts as a failed call toward its breaker. One that answers with a
    /// protocol version, capability or hook set this Guardian cannot work with opens its breaker
    /// at once: its decisions are not trusted until a probe passes again.
    pub async fn probe(&self) {
        let Ok(rows) = self.rows().await else {
            return;
        };
        self.publish(&rows);
        for instance in &self.instances {
            if !Self::enabled_in(&rows, instance) {
                continue;
            }
            let id = instance.config.id.as_str();
            let unchecked = self
                .unchecked
                .lock()
                .is_ok_and(|unchecked| unchecked.contains(id));
            // An instance that started without its startup checks runs all of them here, the
            // params that apply included, before any call is let through.
            let healthy = if unchecked {
                super::registry::check(
                    &instance.config,
                    &instance.module,
                    &Self::global_params_in(&rows, instance),
                )
                .await
                .map(|_| ())
            } else {
                super::registry::describe_healthy(&instance.config, &instance.module)
                    .await
                    .map(|_| ())
            };
            super::metrics::up(id, healthy.is_ok());
            match healthy {
                Ok(()) => {
                    if unchecked {
                        if let Ok(mut unchecked) = self.unchecked.lock() {
                            unchecked.remove(id);
                        }
                        tracing::info!(
                            module = id,
                            "Policy module passed the startup checks it could not be reached for"
                        );
                    }
                    self.breakers.probe_succeeded(id);
                }
                Err(Unhealthy::Unreachable(error)) => {
                    tracing::warn!(module = id, %error, "Policy module health probe failed");
                    self.breakers.failed(id);
                }
                Err(Unhealthy::Incompatible(error) | Unhealthy::InvalidParams(error)) => {
                    tracing::warn!(module = id, %error, "Policy module cannot be used by this Guardian; its circuit is open until a probe passes");
                    self.breakers.hold_open(id);
                }
            }
        }
    }

    /// Runs [`Self::probe`] every `GUARDIAN_MODULE_PROBE_INTERVAL_MS` for the life of the server.
    pub fn spawn_probe(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(self.limits.probe_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut counted = tokio::time::Instant::now();
            loop {
                interval.tick().await;
                if counted.elapsed() >= ASSIGNMENT_REPORT_INTERVAL {
                    self.refresh_assignment_report().await;
                    counted = tokio::time::Instant::now();
                }
                self.probe().await;
            }
        })
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn instance(&self, module_id: &str) -> Option<&Instance> {
        self.instances
            .iter()
            .find(|instance| instance.config.id == module_id)
    }

    #[cfg(test)]
    pub fn store_handle(&self) -> Arc<dyn ModuleStore> {
        self.store.clone()
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

    /// The global rows and one account's rows as of a single params version, read in one store
    /// call. Without it a request could combine a new global rule with an old account layer, a
    /// combination no operator ever configured. The instance rows come from the cache when it
    /// holds the version the store reports.
    pub(super) async fn snapshot(
        &self,
        account_id: &str,
    ) -> Result<(u64, Rows, Vec<AccountParamsRow>), GuardianError> {
        let cached = self
            .cache
            .read()
            .ok()
            .and_then(|cache| cache.version.map(|version| (version, cache.rows.clone())));
        let snapshot = self
            .store
            .snapshot(account_id, cached.as_ref().map(|(version, _)| *version))
            .await
            .map_err(Self::store_error)?;
        let rows = match (snapshot.instances, cached) {
            (Some(instances), _) => {
                let rows: Rows = Arc::new(
                    instances
                        .into_iter()
                        .map(|row| (row.module_id, (row.global_params, row.enabled)))
                        .collect(),
                );
                if let Ok(mut cache) = self.cache.write() {
                    *cache = Cache {
                        version: Some(snapshot.version),
                        rows: rows.clone(),
                    };
                }
                rows
            }
            (None, Some((_, rows))) => rows,
            (None, None) => {
                return Err(Self::store_error(
                    "module store omitted the instance rows without a cached version".into(),
                ));
            }
        };
        Ok((snapshot.version, rows, snapshot.account_params))
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
        Ok(self.plan_with(hook, account_id, false).await?.0)
    }

    /// The `before_account_registered` plan of an account registering for the first time (no
    /// account yet, or a claim with no assignment history): the `all` instances and every
    /// enabled `default` instance subscribed to the hook, with its current global params. Also
    /// returns the `default` copies to store for the account, every enabled `default` instance
    /// with exactly the params evaluated, read in the same snapshot.
    pub async fn plan_first_registration(
        &self,
        account_id: &str,
    ) -> Result<(Option<Plan<'_>>, Vec<Assigned>), GuardianError> {
        self.plan_with(HookName::BeforeAccountRegistered, account_id, true)
            .await
    }

    async fn plan_with(
        &self,
        hook: HookName,
        account_id: &str,
        first_registration: bool,
    ) -> Result<(Option<Plan<'_>>, Vec<Assigned>), GuardianError> {
        let account_id = account_id.to_ascii_lowercase();
        let subscribed = |rows: &Rows, instance: &&Instance| {
            instance.config.hooks.contains(&hook) && Self::enabled_in(rows, instance)
        };
        let relevant = self.instances.iter().any(|instance| {
            instance.config.hooks.contains(&hook)
                || first_registration && instance.config.scope == Scope::Default
        });
        if !relevant {
            return Ok((None, Vec::new()));
        }
        let (params_version, rows, account_rows) = self.snapshot(&account_id).await?;
        let defaults: Vec<Assigned> = if first_registration {
            self.instances
                .iter()
                .filter(|instance| {
                    instance.config.scope == Scope::Default && Self::enabled_in(&rows, instance)
                })
                .map(|instance| Assigned {
                    module_id: instance.config.id.clone(),
                    params: Self::global_params_in(&rows, instance),
                })
                .collect()
        } else {
            Vec::new()
        };
        let active: Vec<&Instance> = self
            .instances
            .iter()
            .filter(|instance| subscribed(&rows, instance))
            .collect();
        if active.is_empty() {
            return Ok((None, defaults));
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
        for copy in &defaults {
            if let Some(instance) = active
                .iter()
                .find(|instance| instance.config.id == copy.module_id)
            {
                modules.push((*instance, copy.params.clone(), Layer::Global));
            }
        }
        let plan = (!modules.is_empty()).then_some(Plan {
            hook,
            account_id,
            modules,
            network_id: self.network_id,
            catalog_hash: self.catalog_hash.clone(),
            params_version,
            views: self.views.clone(),
            limits: self.limits,
            breakers: self.breakers.clone(),
            dry_run: false,
        });
        Ok((plan, defaults))
    }

    /// The plans for a dry-run check of a proposal: the `before_proposal_created` instances,
    /// then those subscribed only to `before_transaction_approved`, so an instance on both runs
    /// once and rules that must hold at approval are checked too. Instances with
    /// `dry_run = false` are skipped. `complete` is false whenever the catalog has an enabled
    /// transaction gate instance that opts out, whether or not it applies to this account, so
    /// the answer reveals nothing about one account's instances.
    pub async fn plan_dry_run(
        &self,
        account_id: &str,
    ) -> Result<(Vec<Plan<'_>>, bool), GuardianError> {
        let (_, rows) = self.versioned_rows().await?;
        let complete = !self.instances.iter().any(|instance| {
            Self::enabled_in(&rows, instance)
                && !instance.config.dry_run
                && (instance
                    .config
                    .hooks
                    .contains(&HookName::BeforeProposalCreated)
                    || instance
                        .config
                        .hooks
                        .contains(&HookName::BeforeTransactionApproved))
        });
        let mut plans = Vec::new();
        for hook in [
            HookName::BeforeProposalCreated,
            HookName::BeforeTransactionApproved,
        ] {
            let Some(mut plan) = self.plan(hook, account_id).await? else {
                continue;
            };
            plan.modules.retain(|(instance, _, _)| {
                instance.config.dry_run
                    && (hook == HookName::BeforeProposalCreated
                        || !instance
                            .config
                            .hooks
                            .contains(&HookName::BeforeProposalCreated))
            });
            plan.dry_run = true;
            if !plan.modules.is_empty() {
                plans.push(plan);
            }
        }
        Ok((plans, complete))
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
    /// The network this Guardian serves, for this plan's request.
    pub fn network_id(&self) -> &'static str {
        self.network_id
    }

    /// The replica's account view cache, for building this plan's request.
    pub fn views(&self) -> Arc<ViewCache> {
        self.views.clone()
    }

    /// Runs the planned modules in order and returns the configuration they ran with. If
    /// `build` fails, every module counts as failed and its `on_error` applies.
    pub async fn run(
        self,
        build: impl FnOnce() -> Result<HookRequest, String>,
    ) -> Result<Evaluation, GuardianError> {
        let Plan {
            hook,
            account_id,
            modules,
            network_id: _,
            catalog_hash,
            params_version,
            views: _,
            limits,
            breakers,
            dry_run,
        } = self;
        let account_id = account_id.as_str();
        let mut approval = Evaluation {
            catalog_hash,
            params_version,
            failed_open: Vec::new(),
        };
        let request_id = format!("{:032x}", rand::random::<u128>());
        let base = match build() {
            Ok(base) => HookRequest {
                request_id: request_id.clone(),
                dry_run,
                ..base
            },
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

        let caller = if dry_run {
            Caller::DryRun
        } else {
            Caller::Request
        };
        let deadline = tokio::time::Instant::now() + limits.budget;
        let mut called_before = false;
        for (instance, params, layer) in modules {
            let id = instance.config.id.as_str();
            let mut request = base.clone();
            request.hook = hook.proto() as i32;
            request.instance_id = id.to_string();
            request.params = serde_json::to_vec(&params).unwrap_or_default();

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let size = prost::Message::encoded_len(&request);
            let unsent = if size > limits.request_max_bytes {
                Some(Failure::RequestTooLarge(size))
            } else if remaining.is_zero() {
                Some(Failure::Timeout)
            } else if !breakers.admit(id, caller) {
                Some(Failure::CircuitOpen)
            } else {
                None
            };
            if let Some(failure) = unsent {
                super::metrics::evaluated(
                    id,
                    super::metrics::Outcome::Skipped,
                    std::time::Duration::ZERO,
                );
                approval
                    .failed_open
                    .extend(fail(instance, layer, account_id, &failure)?);
                continue;
            }

            // A call that earlier modules left less than its timeout counts as a timeout, but is
            // not the module's failure. The first call always has its whole share.
            let cut_by_budget = called_before && remaining < instance.config.timeout;
            called_before = true;
            let module = instance.module.clone();
            let started = tokio::time::Instant::now();
            let outcome = isolated(instance.config.timeout.min(remaining), async move {
                module.evaluate(&request).await
            })
            .await;
            let took = started.elapsed();
            let failure = match outcome.map(|decision| (effect(&decision), decision)) {
                Ok((Some(Effect::Allow), _)) => {
                    if !dry_run {
                        breakers.answered(id);
                    }
                    super::metrics::evaluated(id, super::metrics::Outcome::Allow, took);
                    continue;
                }
                Ok((Some(Effect::Deny), decision))
                    if is_valid_reason_code(&decision.reason_code) =>
                {
                    if !dry_run {
                        breakers.answered(id);
                    }
                    super::metrics::evaluated(id, super::metrics::Outcome::Deny, took);
                    tracing::info!(
                        request_id = %request_id,
                        module = %instance.config.id,
                        account_id,
                        ?hook,
                        submitter = %base.submitter,
                        reason_code = %decision.reason_code,
                        layer = layer.as_str(),
                        message = %decision.message,
                        catalog_hash = %approval.catalog_hash,
                        params_version = approval.params_version,
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
                Err(CallError::Module(guardian_module_sdk::ModuleError::InvalidRequest(
                    message,
                ))) => Failure::Rejected(message),
                Err(CallError::Module(error)) => Failure::Error(error.to_string()),
                Err(CallError::Panicked(message)) => {
                    Failure::Error(format!("module panicked: {message}"))
                }
                Err(CallError::Timeout) => Failure::Timeout,
            };
            super::metrics::evaluated(id, super::metrics::Outcome::Error, took);
            // The breaker stops calls to a module that is down for every account, so only
            // failures that say so count: not a dry run's, which a cosigner sends without anyone's
            // signature, not a request the module rejected, and not a budget cut.
            if !dry_run {
                match failure {
                    Failure::Rejected(_) => breakers.answered(id),
                    Failure::Timeout if cut_by_budget => {}
                    _ => breakers.failed(id),
                }
            }
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

/// What a decision asks for: `None` for a decision that sets no effect or one this Guardian
/// does not know, neither of which is ever read as an allow.
fn effect(decision: &Decision) -> Option<Effect> {
    match Effect::try_from(decision.effect) {
        Ok(Effect::Unspecified) | Err(_) => None,
        Ok(effect) => Some(effect),
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
                module: match instance.config.client_reason {
                    ClientReason::Full => Some(instance.config.id.clone()),
                    ClientReason::Generic => None,
                },
            })
        }
        OnError::Allow => {
            super::metrics::failed_open(&instance.config.id);
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
    async fn a_probe_publishes_the_state_gauges() {
        let pipeline = pipeline(vec![instance(
            "caps",
            &[HookName::BeforeTransactionApproved],
            OnError::Deny,
            StubModule::allowing(),
        )]);
        let rows = pipeline.rows().await.unwrap();
        let recorder = crate::metrics::build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || pipeline.publish(&rows));
        let text = handle.render();
        for line in [
            "guardian_module_enabled{module=\"caps\"} 1",
            "guardian_module_circuit_open{module=\"caps\"} 0",
            "guardian_module_config_drift{module=\"caps\"} 0",
            "guardian_module_over_budget_accounts 0",
            "guardian_module_orphaned_assignments 0",
            "guardian_module_catalog_info{catalog_hash=\"\"} 1",
        ] {
            assert!(text.contains(line), "{line} missing from:\n{text}");
        }
    }

    #[tokio::test]
    async fn invalid_catalog_params_stop_startup_without_seeding_them() {
        let store: Arc<dyn ModuleStore> = Arc::new(super::super::store::FsModuleStore::in_memory());
        let registry = Registry::first_party();
        let now = "2026-10-15T00:00:00Z".to_string();
        let started = ModulePipeline::new(
            "local",
            caps_catalog(r#"{ max_amount_per_txx = {} }"#),
            &registry,
            store.clone(),
            now.clone(),
            crate::modules::limits::Limits::default(),
        )
        .await;
        assert!(started.is_err());
        assert!(store.instances().await.unwrap().is_empty());

        let fixed =
            caps_catalog(r#"{ max_amount_per_tx = { "0x0000000000000000000000000000aa" = "5" } }"#);
        let started = ModulePipeline::new(
            "local",
            fixed,
            &registry,
            store.clone(),
            now,
            crate::modules::limits::Limits::default(),
        )
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
            "local",
            caps_catalog(r#"{ max_amount_per_txx = {} }"#),
            &registry,
            store.clone(),
            now.clone(),
            crate::modules::limits::Limits::default(),
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
            ModulePipeline::new(
                "local",
                valid_seed,
                &registry,
                store,
                now,
                crate::modules::limits::Limits::default()
            )
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
            Err(GuardianError::ModuleUnavailable { module }) if module.as_deref() == Some("slow")
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
            Err(GuardianError::ModuleUnavailable { module }) if module.as_deref() == Some("buggy")
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

    fn decision(reason_code: &str, effect: i32) -> Behaviour {
        Behaviour::Decide(Decision {
            reason_code: reason_code.into(),
            effect,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn the_effect_decides_and_a_missing_or_unknown_one_is_a_failure() {
        assert!(matches!(
            run(&pipeline_with(
                OnError::Deny,
                decision("explicit_deny", Effect::Deny as i32)
            ))
            .await,
            Err(GuardianError::ModuleDenied { .. })
        ));
        assert!(
            run(&pipeline_with(
                OnError::Deny,
                decision("", Effect::Allow as i32)
            ))
            .await
            .is_ok()
        );
        for effect in [Effect::Unspecified as i32, 99] {
            assert!(
                matches!(
                    run(&pipeline_with(OnError::Deny, decision("", effect))).await,
                    Err(GuardianError::ModuleUnavailable { .. })
                ),
                "effect {effect} is never read as an allow"
            );
        }
    }

    #[tokio::test]
    async fn normalized_params_that_break_the_params_rules_are_not_stored() {
        use crate::modules::testing::{FixedFactory, ProtocolStub};
        let catalog = Catalog::parse(
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
        .unwrap();
        for normalized in [
            serde_json::json!({ "new_name": "x".repeat(200) }),
            serde_json::json!({ "key": { "$secret": "vendor-key" } }),
        ] {
            let mut stub = ProtocolStub::new();
            stub.normalized = Some(normalized.clone());
            let mut registry = Registry::first_party();
            registry
                .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
                .unwrap();
            let store: Arc<dyn ModuleStore> =
                Arc::new(super::super::store::FsModuleStore::in_memory());
            ModulePipeline::new(
                "local",
                catalog.clone(),
                &registry,
                store.clone(),
                "2026-10-16T00:00:00Z".to_string(),
                crate::modules::limits::Limits {
                    params_max_bytes: 100,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert_eq!(
                store.instances().await.unwrap()[0].global_params,
                serde_json::json!({ "old_name": 5 }),
                "kept the catalog params instead of {normalized}"
            );
        }
    }

    const OUTAGE_CATALOG: &str = r#"
[[module]]
id = "screening"
kind = "outage-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
"#;

    async fn start_with(
        module: Arc<crate::modules::testing::Outage>,
        catalog: &str,
        store: Arc<dyn ModuleStore>,
    ) -> Result<ModulePipeline, String> {
        let mut registry = Registry::first_party();
        registry
            .register(
                "outage-stub",
                Arc::new(crate::modules::testing::FixedFactory(module)),
            )
            .unwrap();
        ModulePipeline::new(
            "local",
            Catalog::parse(catalog, None).unwrap(),
            &registry,
            store,
            "2026-11-10T00:00:00Z".to_string(),
            crate::modules::limits::Limits::default(),
        )
        .await
    }

    #[tokio::test]
    async fn a_known_instance_whose_module_is_down_starts_unavailable_and_recovers_on_a_probe() {
        use std::sync::atomic::Ordering;
        let module = crate::modules::testing::Outage::new(false);
        let store: Arc<dyn ModuleStore> = Arc::new(super::super::store::FsModuleStore::in_memory());
        start_with(module.clone(), OUTAGE_CATALOG, store.clone())
            .await
            .expect("the first start passes the checks and makes the instance known");

        module.set_down(true);
        let pipeline = start_with(module.clone(), OUTAGE_CATALOG, store)
            .await
            .expect("a module outage does not keep the Guardian from restarting");
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
        assert!(
            matches!(
                run(&pipeline).await,
                Err(GuardianError::ModuleUnavailable { .. })
            ),
            "its accounts are refused meanwhile, not approved"
        );

        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Open,
            "still down"
        );

        module.set_down(false);
        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::HalfOpen
        );
        run(&pipeline).await.expect("the module decides again");
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Closed);
        assert_eq!(module.evaluations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_instance_new_to_this_guardian_must_answer_at_startup() {
        let module = crate::modules::testing::Outage::new(true);
        let store: Arc<dyn ModuleStore> = Arc::new(super::super::store::FsModuleStore::in_memory());
        let refused = start_with(module, OUTAGE_CATALOG, store.clone())
            .await
            .err()
            .expect("a wrong endpoint in a new catalog entry is found at deploy time");
        assert!(refused.contains("screening"), "{refused}");
        assert!(store.instances().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_known_instance_that_answers_incompatibly_still_stops_startup() {
        use crate::modules::testing::{FixedFactory, ProtocolStub};
        const CATALOG: &str = r#"
[[module]]
id = "screening"
kind = "protocol-stub"
scope = "all"
hooks = ["before_transaction_approved"]
timeout_ms = 50
"#;
        let start = |protocol_version: u32, store: Arc<dyn ModuleStore>| async move {
            let mut stub = ProtocolStub::new();
            stub.protocol_version = protocol_version;
            let mut registry = Registry::first_party();
            registry
                .register("protocol-stub", Arc::new(FixedFactory(Arc::new(stub))))
                .unwrap();
            ModulePipeline::new(
                "local",
                Catalog::parse(CATALOG, None).unwrap(),
                &registry,
                store,
                "2026-11-10T00:00:00Z".to_string(),
                crate::modules::limits::Limits::default(),
            )
            .await
        };
        let store: Arc<dyn ModuleStore> = Arc::new(super::super::store::FsModuleStore::in_memory());
        start(guardian_module_sdk::PROTOCOL_VERSION, store.clone())
            .await
            .unwrap();
        assert!(
            start(guardian_module_sdk::PROTOCOL_VERSION + 1, store)
                .await
                .is_err(),
            "only a module that does not answer may start unavailable"
        );
    }

    #[tokio::test]
    async fn an_instance_the_catalog_requires_is_enabled_at_startup() {
        let disabled_in_store = || async {
            let store: Arc<dyn ModuleStore> =
                Arc::new(super::super::store::FsModuleStore::in_memory());
            store
                .seed_instance("screening", false, &serde_json::json!({}), "t0")
                .await
                .unwrap();
            store
        };

        let store = disabled_in_store().await;
        let pipeline = start_with(
            crate::modules::testing::Outage::new(false),
            OUTAGE_CATALOG,
            store.clone(),
        )
        .await
        .unwrap();
        assert!(
            store.instances().await.unwrap()[0].enabled,
            "enabled and not operator-disableable in the catalog: a stored disable does not stand"
        );
        let history = store.history("screening", None, 10).await.unwrap();
        assert_eq!(history[0].operator, "catalog");
        assert_eq!(history[0].new, serde_json::json!({ "enabled": true }));
        assert!(
            pipeline
                .applies(HookName::BeforeTransactionApproved, "0xacc")
                .await
                .unwrap()
        );

        let store = disabled_in_store().await;
        let disableable = format!("{OUTAGE_CATALOG}operator_can_disable = true\n");
        let pipeline = start_with(
            crate::modules::testing::Outage::new(false),
            &disableable,
            store.clone(),
        )
        .await
        .unwrap();
        assert!(
            !store.instances().await.unwrap()[0].enabled,
            "an operator may hold a disableable instance off, so the stored flag stands"
        );
        assert!(
            !pipeline
                .applies(HookName::BeforeTransactionApproved, "0xacc")
                .await
                .unwrap()
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

        ModulePipeline::new(
            "local",
            catalog(),
            &registry,
            store.clone(),
            now.clone(),
            crate::modules::limits::Limits::default(),
        )
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
        ModulePipeline::new(
            "local",
            catalog(),
            &registry,
            store.clone(),
            now,
            crate::modules::limits::Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            store.instances().await.unwrap()[0].global_params,
            serde_json::json!({ "new_name": 5 })
        );
        let last = store.history("migrating", None, 1).await.unwrap().remove(0);
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
            "local",
            catalog,
            &registry,
            store.clone(),
            "2026-10-16T00:00:02Z".into(),
            crate::modules::limits::Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            store.instances().await.unwrap()[0].global_params,
            serde_json::json!({ "new_name": 100 }),
            "the operator's cap of 100 stays"
        );
        let last = store.history("migrating", None, 1).await.unwrap().remove(0);
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
        use guardian_module_sdk::{
            MidenTransaction, OutputNote, TransactionProjection, TransactionSubject,
        };
        use serde_json::json;

        fn pay(recipient: &str) -> HookRequest {
            HookRequest {
                subject: Some(hook_request::Subject::Transaction(TransactionSubject {
                    projection: Some(TransactionProjection::miden(MidenTransaction {
                        output_notes: vec![OutputNote {
                            note_id: "0xn".into(),
                            p2id_target: Some(recipient.into()),
                            ..Default::default()
                        }],
                        ..Default::default()
                    })),
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
                json!({ "recipient_denylist": ["0x000000000000000000000000000aaa"] }),
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
                    Some(json!({ "recipient_denylist": ["0x000000000000000000000000000ccc"] })),
                    &change(),
                )
                .await
                .unwrap();

            assert_eq!(
                layer_of(&pipeline, "0xacc", "0x000000000000000000000000000aaa")
                    .await
                    .as_deref(),
                Some("global")
            );
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0x000000000000000000000000000ccc")
                    .await
                    .as_deref(),
                Some("account")
            );
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0x000000000000000000000000000bbb").await,
                None
            );
            assert_eq!(
                layer_of(&pipeline, "0xother", "0x000000000000000000000000000ccc").await,
                None
            );
            assert_eq!(
                layer_of(&pipeline, "0xother", "0x000000000000000000000000000aaa")
                    .await
                    .as_deref(),
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
                layer_of(&pipeline, "0xacc", "0x000000000000000000000000000aaa")
                    .await
                    .as_deref(),
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
                    &json!({ "recipient_denylist": ["0x000000000000000000000000000aaa"] }),
                    "t0",
                )
                .await
                .unwrap();
            pipeline
                .store()
                .set_global(
                    "blocklist",
                    json!({ "recipient_denylist": ["0x000000000000000000000000000aaa", "0x000000000000000000000000000ddd"] }),
                    &change(),
                )
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xany", "0x000000000000000000000000000ddd")
                    .await
                    .as_deref(),
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
                    &json!({ "recipient_denylist": ["0x000000000000000000000000000aaa"] }),
                    "t0",
                )
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0x000000000000000000000000000aaa")
                    .await
                    .as_deref(),
                Some("global")
            );
            pipeline
                .store()
                .set_enabled("blocklist", false, &change())
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0x000000000000000000000000000aaa").await,
                None
            );
            assert!(!pipeline.applies(HOOK, "0xacc").await.unwrap());
            pipeline
                .store()
                .set_enabled("blocklist", true, &change())
                .await
                .unwrap();
            assert_eq!(
                layer_of(&pipeline, "0xacc", "0x000000000000000000000000000aaa")
                    .await
                    .as_deref(),
                Some("global")
            );
        }
    }
}

#[cfg(test)]
mod evaluation_trace_tests {
    use guardian_module_sdk::HookRequest;

    use super::*;
    use crate::modules::catalog::{ClientReason, HookName, OnError};
    use crate::modules::testing::{StubModule, instance};

    const HOOK: HookName = HookName::BeforeTransactionApproved;

    #[tokio::test]
    async fn one_invocation_shares_a_request_id_and_the_next_gets_a_new_one() {
        let first = StubModule::allowing();
        let second = StubModule::allowing();
        let pipeline = ModulePipeline::from_instances(vec![
            instance("first", &[HOOK], OnError::Deny, first.clone()),
            instance("second", &[HOOK], OnError::Deny, second.clone()),
        ]);
        let id = |module: &StubModule| module.last_request().unwrap().request_id;

        pipeline
            .run(HOOK, "0xacc", || Ok(HookRequest::default()))
            .await
            .unwrap();
        let invocation = id(&first);
        assert!(!invocation.is_empty());
        assert_eq!(id(&second), invocation);

        pipeline
            .run(HOOK, "0xacc", || Ok(HookRequest::default()))
            .await
            .unwrap();
        assert_ne!(id(&first), invocation);
    }

    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_generic_denial_hides_the_decision_from_the_client_but_logs_it_whole() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(captured.clone())
            .with_ansi(false)
            .finish();
        let _pin = crate::testing::log_capture::dispatcher_registry_pin();
        let _default = tracing::subscriber::set_default(subscriber);

        let mut screening = instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            StubModule::denying("sanctioned_counterparty"),
        );
        screening.config.client_reason = ClientReason::Generic;
        let pipeline = ModulePipeline::from_instances(vec![screening]);
        let result = pipeline
            .run(HOOK, "0xacc", || {
                Ok(HookRequest {
                    submitter: "0xsubmitter".into(),
                    ..Default::default()
                })
            })
            .await;
        assert!(matches!(result, Err(GuardianError::ModuleDeniedGeneric)));

        let log = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        for field in [
            "request_id=",
            "module=screening",
            "account_id=\"0xacc\"",
            "submitter=0xsubmitter",
            "reason_code=sanctioned_counterparty",
            "layer=\"global\"",
            "catalog_hash=",
            "params_version=",
        ] {
            assert!(log.contains(field), "{field} missing from {log}");
        }
    }
}

#[cfg(test)]
mod failure_handling_tests {
    use std::time::Duration;

    use guardian_module_sdk::HookRequest;

    use super::*;
    use crate::modules::breaker::CircuitState;
    use crate::modules::catalog::{HookName, OnError};
    use crate::modules::testing::{Behaviour, StubModule, instance};

    const HOOK: HookName = HookName::BeforeTransactionApproved;

    fn hanging(id: &str, on_error: OnError, timeout: Duration) -> (Instance, Arc<StubModule>) {
        let module = StubModule::new(Behaviour::Hang);
        let mut hung = instance(id, &[HOOK], on_error, module.clone());
        hung.config.timeout = timeout;
        (hung, module)
    }

    async fn run(pipeline: &ModulePipeline) -> Result<(), GuardianError> {
        pipeline
            .run(HOOK, "0xacc", || Ok(HookRequest::default()))
            .await
    }

    #[tokio::test]
    async fn a_failing_fail_closed_module_refuses_retryably_and_a_fail_open_one_is_skipped() {
        let closed = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Fail),
        )]);
        let refused = run(&closed).await.unwrap_err();
        assert!(
            matches!(&refused, GuardianError::ModuleUnavailable { module } if module.as_deref() == Some("screening"))
        );
        assert!(refused.retryable());

        let mut hidden = instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Fail),
        );
        hidden.config.client_reason = ClientReason::Generic;
        let refused = run(&ModulePipeline::from_instances(vec![hidden]))
            .await
            .unwrap_err();
        assert!(
            matches!(&refused, GuardianError::ModuleUnavailable { module: None }),
            "an instance with generic denials is not named when it fails either: {refused:?}"
        );

        let open = ModulePipeline::from_instances(vec![instance(
            "scoring",
            &[HOOK],
            OnError::Allow,
            StubModule::new(Behaviour::Fail),
        )]);
        assert!(run(&open).await.is_ok());
    }

    #[tokio::test]
    async fn dry_run_failures_do_not_open_the_circuit() {
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Fail),
        )])
        .with_limits(Limits {
            breaker_failures: 2,
            ..Limits::default()
        });
        for _ in 0..4 {
            let (plans, _) = pipeline.plan_dry_run("0xacc").await.unwrap();
            for plan in plans {
                assert!(plan.run(|| Ok(HookRequest::default())).await.is_err());
            }
        }
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Closed);
    }

    #[tokio::test]
    async fn a_request_the_module_rejects_does_not_open_the_circuit() {
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Reject),
        )])
        .with_limits(Limits {
            breaker_failures: 2,
            ..Limits::default()
        });
        for _ in 0..4 {
            assert!(matches!(
                run(&pipeline).await,
                Err(GuardianError::ModuleUnavailable { .. })
            ));
        }
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Closed,
            "one account's unjudgeable requests do not stop the module for every account"
        );
    }

    #[tokio::test]
    async fn a_module_whose_timeout_is_the_whole_budget_still_opens_the_circuit() {
        let (hung, _) = hanging("screening", OnError::Deny, Duration::from_millis(40));
        let pipeline = ModulePipeline::from_instances(vec![hung]).with_limits(Limits {
            budget: Duration::from_millis(40),
            breaker_failures: 2,
            ..Limits::default()
        });
        for _ in 0..2 {
            assert!(run(&pipeline).await.is_err());
        }
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
    }

    #[tokio::test]
    async fn consecutive_failures_open_the_circuit_so_later_requests_skip_the_timeout() {
        let (hung, module) = hanging("screening", OnError::Deny, Duration::from_millis(50));
        let pipeline = ModulePipeline::from_instances(vec![hung]).with_limits(Limits {
            breaker_failures: 2,
            ..Limits::default()
        });
        for _ in 0..2 {
            assert!(run(&pipeline).await.is_err());
        }
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);

        let started = tokio::time::Instant::now();
        let refused = run(&pipeline).await.unwrap_err();
        assert!(matches!(refused, GuardianError::ModuleUnavailable { .. }));
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "an open circuit refuses without waiting the timeout"
        );
        assert_eq!(module.calls(), 2, "the module is not called while open");

        pipeline.breakers().probe_succeeded("screening");
        assert!(run(&pipeline).await.is_err());
        assert_eq!(module.calls(), 3, "half-open sends the next call");
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
    }

    #[tokio::test]
    async fn the_budget_cuts_later_modules_short_without_blaming_them() {
        let (first, _) = hanging("first", OnError::Allow, Duration::from_millis(80));
        let (second, second_module) = hanging("second", OnError::Allow, Duration::from_millis(80));
        let pipeline = ModulePipeline::from_instances(vec![first, second]).with_limits(Limits {
            budget: Duration::from_millis(100),
            breaker_failures: 1,
            ..Limits::default()
        });
        let started = tokio::time::Instant::now();
        assert!(run(&pipeline).await.is_ok(), "both fail open");
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "the second call gets only what is left of the budget"
        );
        assert_eq!(second_module.calls(), 1);
        assert_eq!(pipeline.breakers().state("first"), CircuitState::Open);
        assert_eq!(
            pipeline.breakers().state("second"),
            CircuitState::Closed,
            "a call cut short by the budget is not the module's failure"
        );
    }

    #[tokio::test]
    async fn an_oversized_request_is_a_module_failure_and_is_not_sent() {
        let module = StubModule::allowing();
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            module.clone(),
        )])
        .with_limits(Limits {
            request_max_bytes: 64,
            ..Limits::default()
        });
        let refused = pipeline
            .run(HOOK, "0xacc", || {
                Ok(HookRequest {
                    submitter: "0x".repeat(64),
                    ..Default::default()
                })
            })
            .await;
        assert!(matches!(
            refused,
            Err(GuardianError::ModuleUnavailable { .. })
        ));
        assert_eq!(module.calls(), 0);
    }

    #[tokio::test]
    async fn a_fail_open_failure_does_not_stop_a_later_deny() {
        let pipeline = ModulePipeline::from_instances(vec![
            instance(
                "scoring",
                &[HOOK],
                OnError::Allow,
                StubModule::new(Behaviour::Fail),
            ),
            instance(
                "caps",
                &[HOOK],
                OnError::Deny,
                StubModule::denying("amount_cap_exceeded"),
            ),
        ]);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleDenied { module, .. }) if module == "caps"
        ));
    }

    #[tokio::test]
    async fn a_module_that_turns_incompatible_is_not_trusted_until_a_probe_passes() {
        let mut stub = crate::modules::testing::ProtocolStub::new();
        stub.protocol_version = guardian_module_sdk::PROTOCOL_VERSION + 1;
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            Arc::new(stub),
        )]);
        run(&pipeline)
            .await
            .expect("the module allows until a probe finds it incompatible");
        pipeline.probe().await;
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleUnavailable { .. })
        ));
        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Open,
            "it stays open while the probe keeps failing"
        );
    }

    /// Allows every request but never answers `Describe`.
    struct Undescribed;

    #[async_trait::async_trait]
    impl guardian_module_sdk::Module for Undescribed {
        async fn describe(
            &self,
            _: &guardian_module_sdk::DescribeRequest,
        ) -> Result<guardian_module_sdk::ModuleInfo, guardian_module_sdk::ModuleError> {
            Err(guardian_module_sdk::ModuleError::Unavailable(
                "no answer".into(),
            ))
        }

        async fn validate_params(
            &self,
            _: &[u8],
        ) -> Result<guardian_module_sdk::ParamsVerdict, guardian_module_sdk::ModuleError> {
            Ok(guardian_module_sdk::ParamsVerdict::ok())
        }

        async fn evaluate(
            &self,
            _: &HookRequest,
        ) -> Result<guardian_module_sdk::Decision, guardian_module_sdk::ModuleError> {
            Ok(guardian_module_sdk::Decision::allow())
        }
    }

    #[tokio::test]
    async fn probes_a_module_does_not_answer_count_toward_its_breaker() {
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            Arc::new(Undescribed),
        )])
        .with_limits(Limits {
            breaker_failures: 2,
            ..Limits::default()
        });
        pipeline.probe().await;
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Closed);
        pipeline.probe().await;
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Open,
            "no request had to find the module down"
        );
    }

    #[tokio::test]
    async fn a_healthy_probe_half_opens_and_a_failing_one_leaves_the_circuit_open() {
        let healthy = crate::modules::testing::instance_with_params(
            "caps",
            &[HOOK],
            Arc::new(guardian_module_sdk::basic_policy::BasicPolicy),
            serde_json::json!({}),
        );
        let (down, _) = hanging("screening", OnError::Deny, Duration::from_millis(20));
        let pipeline = ModulePipeline::from_instances(vec![healthy, down]).with_limits(Limits {
            breaker_failures: 1,
            ..Limits::default()
        });
        pipeline.breakers().failed("caps");
        pipeline.breakers().failed("screening");
        pipeline.probe().await;
        assert_eq!(pipeline.breakers().state("caps"), CircuitState::HalfOpen);
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
    }
}
