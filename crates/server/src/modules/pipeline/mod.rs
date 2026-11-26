//! Ordered evaluation of an account's modules at one hook.

use guardian_module_sdk::{Decision, Effect, HookRequest, is_valid_reason_code};

mod budget;
mod probe;
mod startup;

use super::catalog::{ClientReason, HookName, OnError, Scope};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use super::breaker::{Breakers, Call, Caller};
use super::limits::Limits;
use super::registry::{CallError, Instance, isolated};
use super::store::{AccountParamsRow, Assigned, ModuleStore, ParamsKind};
use crate::delta_summary::FailedOpen;
use crate::error::GuardianError;

/// The configuration a pipeline run evaluated with, and the failures `on_error = allow` let
/// through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evaluation {
    pub catalog_hash: String,
    /// The instance params version: global params and enabled flags.
    pub params_version: u64,
    /// The account's params version: its layers and assignment.
    pub account_params_version: u64,
    pub failed_open: Vec<FailedOpen>,
}

impl Evaluation {
    /// Records the failures `on_error = allow` let through in the audit log, so they are kept
    /// somewhere other than the log: at proposal creation and registration, which sign nothing,
    /// and at approval once the Guardian signed (they are also stored with the signed delta).
    /// `subject` adds what the hook judged: a proposal id, or a delta's nonce and base.
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
            "account_params_version": self.account_params_version,
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

/// What one evaluation reads: see [`ModulePipeline::snapshot`].
pub(super) struct Snapshot {
    pub params_version: u64,
    pub account_params_version: u64,
    pub rows: Rows,
    pub account_rows: Vec<AccountParamsRow>,
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
    account_params_version: u64,
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
    /// No `effect`, or a value this Guardian does not know: never read as an allow.
    UnknownEffect(i32),
    /// The request could not be built: an account view or the projection could not be
    /// extracted, or the stored state could not be read. Never let through by `on_error`.
    RequestUnavailable(String),
    /// The encoded request exceeds the size cap; it is not sent. Never let through by
    /// `on_error`.
    RequestTooLarge(usize),
    /// The instance's breaker is open; the module is not called.
    CircuitOpen,
}

impl Failure {
    fn kind(&self) -> &'static str {
        match self {
            Failure::Error(_) => "error",
            Failure::Rejected(_) => "rejected",
            Failure::Timeout => "timeout",
            Failure::UnknownEffect(_) => "invalid_decision",
            Failure::RequestUnavailable(_) | Failure::RequestTooLarge(_) => "request_unavailable",
            Failure::CircuitOpen => "circuit_open",
        }
    }

    /// The refusal for a failure that depends on the request rather than on the module,
    /// whatever the instance's `on_error`: a request that cannot be built, that exceeds the size
    /// cap, or that the module rejects as one it cannot judge. `on_error` covers only the
    /// module's own failures. A request that could not be built or that the module rejected is
    /// refused as unavailable, since its cause can be transient (the chain fee not loaded yet, a
    /// module upgrade); one over the cap is denied, since resending it cannot succeed. Returns
    /// the refusal and its `reason` for `guardian_module_request_refused_total`, or `None` for
    /// the module's own failures.
    fn refusal(&self, instance: &Instance, layer: Layer) -> Option<(GuardianError, &'static str)> {
        let generic = instance.config.client_reason == ClientReason::Generic;
        let unavailable = || GuardianError::ModuleUnavailable {
            module: (!generic).then(|| instance.config.id.clone()),
        };
        match self {
            Failure::RequestUnavailable(_) => Some((unavailable(), "unavailable")),
            Failure::Rejected(_) => Some((unavailable(), "rejected")),
            Failure::RequestTooLarge(_) if generic => {
                Some((GuardianError::ModuleDeniedGeneric, "too_large"))
            }
            Failure::RequestTooLarge(_) => Some((
                GuardianError::ModuleDenied {
                    module: instance.config.id.clone(),
                    reason_code: REQUEST_TOO_LARGE.to_string(),
                    layer: layer.as_str().to_string(),
                    client_message: Some(REQUEST_TOO_LARGE_MESSAGE.to_string()),
                },
                "too_large",
            )),
            Failure::Error(_)
            | Failure::Timeout
            | Failure::UnknownEffect(_)
            | Failure::CircuitOpen => None,
        }
    }
}

/// The reason code of the denial for a request over `GUARDIAN_MODULE_REQUEST_MAX_BYTES`.
const REQUEST_TOO_LARGE: &str = "request_too_large";
const REQUEST_TOO_LARGE_MESSAGE: &str =
    "This request is too large for this Guardian's policy modules to judge.";

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Error(message) => write!(f, "{message}"),
            Failure::Rejected(message) => write!(f, "module rejected the request: {message}"),
            Failure::Timeout => write!(f, "timed out"),
            Failure::UnknownEffect(value) => write!(f, "unknown decision effect {value}"),
            Failure::RequestUnavailable(message) => write!(f, "request unavailable: {message}"),
            Failure::RequestTooLarge(bytes) => {
                write!(f, "request of {bytes} bytes over the size cap")
            }
            Failure::CircuitOpen => write!(f, "circuit open"),
        }
    }
}

impl ModulePipeline {
    /// [`startup::storable`] under this pipeline's limits.
    pub fn storable_normalization(&self, params: &serde_json::Value) -> Result<(), String> {
        startup::storable(&self.limits, params)
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

    /// The instance passed its startup checks on this replica, as an operator's enable runs
    /// them: it is no longer unchecked, and an open breaker lets the next real call through as a
    /// passing probe would, instead of waiting for the next probe.
    pub fn checks_passed(&self, module_id: &str) {
        if let Ok(mut unchecked) = self.unchecked.lock() {
            unchecked.remove(module_id);
        }
        self.breakers.probe_succeeded(module_id);
    }

    pub fn instances(&self) -> &[Instance] {
        &self.instances
    }

    pub fn instance(&self, module_id: &str) -> Option<&Instance> {
        self.instances
            .iter()
            .find(|instance| instance.config.id == module_id)
    }

    /// Whether a stored account row is one evaluation applies: its instance is declared, and a
    /// layer names a `scope = all` instance while an assignment entry names any other. A row
    /// that fails this is left over from an earlier catalog (the instance was removed, or its
    /// `scope` changed) and is reported as orphaned.
    pub fn row_applies(&self, module_id: &str, kind: ParamsKind) -> bool {
        self.instance(module_id).is_some_and(|instance| {
            (kind == ParamsKind::Layer) == (instance.config.scope == Scope::All)
        })
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
    /// instance params version moves. One version read per call.
    pub(super) async fn rows(&self) -> Result<Rows, GuardianError> {
        Ok(self.versioned_rows().await?.1)
    }

    async fn versioned_rows(&self) -> Result<(u64, Rows), GuardianError> {
        let version = self
            .store
            .instance_version()
            .await
            .map_err(Self::store_error)?;
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
        self.cache_rows(version, &rows);
        Ok((version, rows))
    }

    /// Whether the account has module history: any write to its layers or assignment, an
    /// emptied assignment included. Only an account without any receives the `default`
    /// instances, by the same rule the copy at registration applies.
    pub async fn has_account_history(&self, account_id: &str) -> Result<bool, GuardianError> {
        let Snapshot {
            account_params_version,
            account_rows,
            ..
        } = self.snapshot(&account_id.to_ascii_lowercase()).await?;
        Ok(account_params_version != 0
            || account_rows
                .iter()
                .any(|row| row.kind == ParamsKind::Assignment))
    }

    /// The global rows and one account's rows with their two params versions, read in one store
    /// call. Without it a request could combine a new global rule with an old account layer, a
    /// combination no operator ever configured. The instance rows come from the cache when it
    /// holds the instance params version the store reports.
    pub(super) async fn snapshot(&self, account_id: &str) -> Result<Snapshot, GuardianError> {
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
                self.cache_rows(snapshot.instance_version, &rows);
                rows
            }
            (None, Some((_, rows))) => rows,
            (None, None) => {
                return Err(Self::store_error(
                    "module store omitted the instance rows without a cached version".into(),
                ));
            }
        };
        Ok(Snapshot {
            params_version: snapshot.instance_version,
            account_params_version: snapshot.account_version,
            rows,
            account_rows: snapshot.account_params,
        })
    }

    /// Caches the instance rows read at `version`, whatever version the cache held. A slow
    /// request can move it back to an older version, which costs the next request one read of
    /// the rows; refusing to move back would keep rows from before a reset of the version
    /// counter, and the store omits the rows whenever the versions are equal.
    fn cache_rows(&self, version: u64, rows: &Rows) {
        if let Ok(mut cache) = self.cache.write() {
            *cache = Cache {
                version: Some(version),
                rows: rows.clone(),
            };
        }
    }

    pub(super) fn global_params_in(rows: &Rows, instance: &Instance) -> serde_json::Value {
        if instance.config.catalog_managed_params() {
            return instance.config.params.clone();
        }
        rows.get(&instance.config.id).map_or_else(
            || instance.config.params.clone(),
            |(params, _)| params.clone(),
        )
    }

    pub(super) fn enabled_in(rows: &Rows, instance: &Instance) -> bool {
        instance.config.catalog_requires_enabled()
            || rows
                .get(&instance.config.id)
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

    /// Whether the instance is switched on: always when the catalog requires it, otherwise the
    /// stored runtime flag, or the startup flag for an instance without a row yet.
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
    /// returns the `default` copies to store for the account: every `default` instance, a
    /// disabled one included so it applies once re-enabled, with its global params as read in
    /// the same snapshot.
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
        let relevant = self.instances.iter().any(|instance| {
            instance.config.hooks.contains(&hook)
                || first_registration && instance.config.scope == Scope::Default
        });
        if !relevant {
            return Ok((None, Vec::new()));
        }
        let snapshot = self.snapshot(&account_id).await?;
        Ok(self.plan_in(hook, account_id, first_registration, &snapshot))
    }

    /// The plan for `hook` and the default copies, built from one snapshot the caller read.
    fn plan_in(
        &self,
        hook: HookName,
        account_id: String,
        first_registration: bool,
        snapshot: &Snapshot,
    ) -> (Option<Plan<'_>>, Vec<Assigned>) {
        let subscribed = |rows: &Rows, instance: &&Instance| {
            instance.config.hooks.contains(&hook) && Self::enabled_in(rows, instance)
        };
        let Snapshot {
            params_version,
            account_params_version,
            rows,
            account_rows,
        } = snapshot;
        let defaults: Vec<Assigned> = if first_registration {
            self.instances
                .iter()
                .filter(|instance| instance.config.scope == Scope::Default)
                .map(|instance| Assigned {
                    module_id: instance.config.id.clone(),
                    params: Self::global_params_in(rows, instance),
                })
                .collect()
        } else {
            Vec::new()
        };
        let active: Vec<&Instance> = self
            .instances
            .iter()
            .filter(|instance| subscribed(rows, instance))
            .collect();
        if active.is_empty() {
            return (None, defaults);
        }
        let mut modules = Vec::new();
        for instance in active
            .iter()
            .filter(|instance| instance.config.scope == Scope::All)
        {
            modules.push((
                *instance,
                Self::global_params_in(rows, instance),
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
            params_version: *params_version,
            account_params_version: *account_params_version,
            limits: self.limits,
            breakers: self.breakers.clone(),
            dry_run: false,
        });
        (plan, defaults)
    }

    /// The plans for a dry-run check of a proposal: the `before_proposal_created` instances,
    /// then the `before_transaction_approved` ones, so rules that must hold at approval are
    /// checked too. An instance on both hooks is asked once per hook, since it may decide
    /// differently at each. Instances with
    /// `dry_run = false` are skipped. `complete` is false whenever the catalog has an enabled
    /// transaction gate instance that opts out, whether or not it applies to this account, so
    /// the answer reveals nothing about one account's instances.
    pub async fn plan_dry_run(
        &self,
        account_id: &str,
    ) -> Result<(Vec<Plan<'_>>, bool), GuardianError> {
        const HOOKS: [HookName; 2] = [
            HookName::BeforeProposalCreated,
            HookName::BeforeTransactionApproved,
        ];
        let gates = |instance: &&Instance| {
            HOOKS
                .iter()
                .any(|hook| instance.config.hooks.contains(hook))
        };
        if !self.instances.iter().any(|instance| gates(&instance)) {
            return Ok((Vec::new(), true));
        }
        let account_id = account_id.to_ascii_lowercase();
        let snapshot = self.snapshot(&account_id).await?;
        let complete = !self.instances.iter().any(|instance| {
            Self::enabled_in(&snapshot.rows, instance)
                && !instance.config.dry_run
                && gates(&instance)
        });
        let mut plans = Vec::new();
        for hook in HOOKS {
            let Some(mut plan) = self.plan_in(hook, account_id.clone(), false, &snapshot).0 else {
                continue;
            };
            plan.modules
                .retain(|(instance, _, _)| instance.config.dry_run);
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

    /// Runs the planned modules in order and returns the configuration they ran with. If
    /// `build` fails (an account view or the projection could not be extracted, a proposal or
    /// state read failed), the request is refused as unavailable whatever each module's
    /// `on_error` says (RFC 0002 section 4.2): see [`Failure::refusal`].
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
            account_params_version,
            limits,
            breakers,
            dry_run,
        } = self;
        let account_id = account_id.as_str();
        let mut approval = Evaluation {
            catalog_hash,
            params_version,
            account_params_version,
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
            let admitted = if size > limits.request_max_bytes {
                Err(Failure::RequestTooLarge(size))
            } else if remaining.is_zero() {
                Err(Failure::Timeout)
            } else {
                breakers.call(id, caller).ok_or(Failure::CircuitOpen)
            };
            let call = match admitted {
                Ok(call) => call,
                Err(failure) => {
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
            };

            let cut_by_budget = cut_by_budget(called_before, remaining, instance.config.timeout);
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
                        call.answered();
                    }
                    super::metrics::evaluated(id, super::metrics::Outcome::Allow, took);
                    continue;
                }
                Ok((Some(Effect::Deny), decision)) => {
                    let reason_code = denial_reason_code(&instance.config.id, decision.reason_code);
                    if !dry_run {
                        call.answered();
                    }
                    super::metrics::evaluated(id, super::metrics::Outcome::Deny, took);
                    tracing::info!(
                        request_id = %request_id,
                        module = %instance.config.id,
                        account_id,
                        ?hook,
                        submitter = %base.submitter,
                        reason_code = %reason_code,
                        layer = layer.as_str(),
                        message = %decision.message,
                        catalog_hash = %approval.catalog_hash,
                        params_version = approval.params_version,
                        account_params_version = approval.account_params_version,
                        client_reason = ?instance.config.client_reason,
                        "Policy module denied the request"
                    );
                    return Err(match instance.config.client_reason {
                        ClientReason::Generic => GuardianError::ModuleDeniedGeneric,
                        ClientReason::Full => GuardianError::ModuleDenied {
                            module: instance.config.id.clone(),
                            reason_code,
                            layer: layer.as_str().to_string(),
                            client_message: client_message(&decision.client_message),
                        },
                    });
                }
                Ok((Some(Effect::Unspecified) | None, decision)) => {
                    Failure::UnknownEffect(decision.effect)
                }
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
            if !dry_run {
                count_toward_breaker(call, &failure, cut_by_budget);
            }
            approval
                .failed_open
                .extend(fail(instance, layer, account_id, &failure)?);
        }
        Ok(approval)
    }
}

/// Whether a call that times out did so because earlier modules used up the budget: it was
/// left less than its own timeout. That is a timeout, but not the module's failure. The first
/// call always has its whole share.
fn cut_by_budget(
    called_before: bool,
    remaining: std::time::Duration,
    timeout: std::time::Duration,
) -> bool {
    called_before && remaining < timeout
}

/// Records a failed call of a request (never a dry run's, which a cosigner sends without
/// anyone's signature) on the instance's breaker. The breaker stops calls to a module that is
/// down for every account, so only failures that say so count: a request the module rejected
/// is an answer, and a timeout the budget caused is not the module's.
fn count_toward_breaker(call: Call<'_>, failure: &Failure, cut_by_budget: bool) {
    match failure {
        Failure::Rejected(_) => call.answered(),
        Failure::Timeout if cut_by_budget => {}
        _ => call.failed(),
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

/// The reason code a denial is reported with: the module's own, or the generic one when the
/// module's breaks the reason code rule. An explicit deny stays a deny either way: treating a
/// malformed code as a module failure would let `on_error = "allow"` sign what the module
/// refused.
fn denial_reason_code(module: &str, reason_code: String) -> String {
    if is_valid_reason_code(&reason_code) {
        return reason_code;
    }
    tracing::warn!(module, %reason_code, "Policy module denied with a reason code that breaks the reason code rule; reported as policy_denied");
    crate::error::GENERIC_REASON_CODE.to_string()
}

/// What a decision asks for: `None` for a decision that sets no effect or one this Guardian
/// does not know, neither of which is ever read as an allow.
fn effect(decision: &Decision) -> Option<Effect> {
    match Effect::try_from(decision.effect) {
        Ok(Effect::Unspecified) | Err(_) => None,
        Ok(effect) => Some(effect),
    }
}

/// Refuses a failure the request causes ([`Failure::refusal`]); otherwise applies the
/// instance's `on_error`: a refusal for `deny`, the failed-open record for `allow`.
fn fail(
    instance: &Instance,
    layer: Layer,
    account_id: &str,
    failure: &Failure,
) -> Result<Option<FailedOpen>, GuardianError> {
    if let Some((refusal, reason)) = failure.refusal(instance, layer) {
        super::metrics::request_refused(&instance.config.id, reason);
        tracing::warn!(module = %instance.config.id, account_id, %failure, "Policy module request could not be judged; refusing whatever on_error says");
        return Err(refusal);
    }
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

    #[test]
    fn a_stored_disable_never_switches_off_an_instance_the_catalog_requires() {
        let mut required = instance(
            "m",
            &[HOOK],
            OnError::Deny,
            StubModule::new(Behaviour::Hang),
        );
        required.config.operator_can_disable = false;
        let rows: Rows = Arc::new(HashMap::from([(
            "m".to_string(),
            (serde_json::json!({}), false),
        )]));
        assert!(ModulePipeline::enabled_in(&rows, &required));
        let mut disableable = required.clone();
        disableable.config.operator_can_disable = true;
        assert!(!ModulePipeline::enabled_in(&rows, &disableable));
        let mut catalog_off = required;
        catalog_off.config.catalog_enabled = false;
        assert!(!ModulePipeline::enabled_in(&rows, &catalog_off));
    }

    async fn run(pipeline: &ModulePipeline) -> Result<(), GuardianError> {
        pipeline
            .run(HOOK, "0xacc", || Ok(HookRequest::default()))
            .await
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

    /// A dry-run check plans both hooks from one store read, so its `complete` and both plans
    /// see the same params versions.
    #[tokio::test]
    async fn a_dry_run_plans_both_hooks_from_one_store_read() {
        use crate::modules::testing::RacingStore;
        use std::sync::atomic::Ordering;
        let store = Arc::new(RacingStore::new());
        let pipeline = ModulePipeline::from_instances_with_store(
            vec![instance(
                "screening",
                &[
                    HookName::BeforeProposalCreated,
                    HookName::BeforeTransactionApproved,
                ],
                OnError::Deny,
                StubModule::new(Behaviour::Hang),
            )],
            store.clone(),
        );
        let (plans, complete) = pipeline.plan_dry_run("0xACC").await.unwrap();
        assert_eq!(plans.len(), 2);
        assert!(complete);
        assert_eq!(store.reads.load(Ordering::SeqCst), 1);
    }

    /// The cache always takes the rows just read: after the version counter is reset under a
    /// running replica, rows cached at the old, higher version must not be served once the
    /// counter climbs back to it.
    #[test]
    fn the_instance_cache_takes_the_rows_just_read_even_at_an_older_version() {
        let pipeline = ModulePipeline::from_instances(Vec::new());
        let newer: Rows = Arc::new(HashMap::from([(
            "m".to_string(),
            (serde_json::json!({ "v": 2 }), true),
        )]));
        let older: Rows = Arc::new(HashMap::from([(
            "m".to_string(),
            (serde_json::json!({ "v": 1 }), true),
        )]));
        pipeline.cache_rows(2, &newer);
        pipeline.cache_rows(1, &older);
        let cache = pipeline.cache.read().unwrap();
        assert_eq!(cache.version, Some(1));
        assert_eq!(cache.rows, older);
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
            expected_version: None,
        };
        store
            .inner
            .seed_instance(
                "caps",
                true,
                &serde_json::json!({ "cap": 1000 }),
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
    async fn a_deny_with_a_malformed_reason_code_is_still_a_deny_whatever_on_error_says() {
        for on_error in [OnError::Deny, OnError::Allow] {
            let pipeline = pipeline(vec![instance(
                "bad",
                &[HOOK],
                on_error,
                StubModule::new(Behaviour::Decide(Decision::deny("RECIPIENT-Blocked", ""))),
            )]);
            match run(&pipeline).await {
                Err(GuardianError::ModuleDenied {
                    module,
                    reason_code,
                    ..
                }) => {
                    assert_eq!(module, "bad");
                    assert_eq!(reason_code, crate::error::GENERIC_REASON_CODE);
                }
                other => panic!("{on_error:?}: expected a denial, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_request_that_cannot_be_built_is_refused_whatever_on_error_says() {
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
            matches!(
                open.run(HOOK, "0xacc", || Err("view failed".into())).await,
                Err(GuardianError::ModuleUnavailable { .. })
            ),
            "a requester must not be able to switch a fail-open module off by making the \
             request unbuildable"
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
        use guardian_module_sdk::policies::RecipientPolicy;
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
                std::sync::Arc::new(RecipientPolicy),
                json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
            )])
        }

        fn change() -> Change<'static> {
            Change {
                operator: "0xop",
                reason: "test",
                at: "t".into(),
                expected_version: None,
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
                    Some(json!({ "denylist": ["0x000000000000000000000000000ccc"] })),
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
                    &json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
                    &json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
                    "t0",
                )
                .await
                .unwrap();
            pipeline
                .store()
                .set_global(
                    "blocklist",
                    json!({ "denylist": ["0x000000000000000000000000000aaa", "0x000000000000000000000000000ddd"] }),
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
                    &json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
                    &json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
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
    async fn a_request_the_module_rejects_is_refused_even_by_a_fail_open_instance() {
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Allow,
            StubModule::new(Behaviour::Reject),
        )]);
        assert!(matches!(
            run(&pipeline).await,
            Err(GuardianError::ModuleUnavailable { module: Some(module) }) if module == "screening"
        ));
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Closed);
    }

    #[tokio::test]
    async fn a_module_whose_timeout_is_the_whole_budget_still_opens_the_circuit() {
        let (hung, _) = hanging("screening", OnError::Deny, Duration::from_millis(40));
        let pipeline = ModulePipeline::from_instances(vec![hung]).with_limits(Limits {
            budget: Duration::from_millis(40),
            breaker_failures: 2,
            ..Limits::default()
        });
        pipeline.breakers().probe_failed("screening");
        assert!(run(&pipeline).await.is_err());
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
    }

    #[tokio::test]
    async fn consecutive_failures_open_the_circuit_so_later_requests_skip_the_timeout() {
        let (hung, module) = hanging("screening", OnError::Deny, Duration::from_millis(50));
        let pipeline = ModulePipeline::from_instances(vec![hung]).with_limits(Limits {
            breaker_failures: 2,
            ..Limits::default()
        });
        assert!(run(&pipeline).await.is_err());
        assert!(run(&pipeline).await.is_err());
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Closed,
            "request failures alone do not open it"
        );
        pipeline.breakers().probe_failed("screening");
        assert!(run(&pipeline).await.is_err());
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);

        let started = tokio::time::Instant::now();
        let refused = run(&pipeline).await.unwrap_err();
        assert!(matches!(refused, GuardianError::ModuleUnavailable { .. }));
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "an open circuit refuses without waiting the timeout"
        );
        assert_eq!(module.calls(), 3, "the module is not called while open");

        pipeline.breakers().probe_succeeded("screening");
        assert!(run(&pipeline).await.is_err());
        assert_eq!(module.calls(), 4, "half-open sends the next call");
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Closed,
            "the module answers its probe, so the failed trial is counted, not trusted"
        );
        pipeline.breakers().probe_failed("screening");
        assert!(run(&pipeline).await.is_err());
        assert_eq!(pipeline.breakers().state("screening"), CircuitState::Open);
    }

    #[tokio::test]
    async fn the_budget_cuts_later_modules_short_without_blaming_them() {
        let (first, _) = hanging("first", OnError::Allow, Duration::from_millis(80));
        let (second, second_module) = hanging("second", OnError::Allow, Duration::from_millis(80));
        let pipeline = ModulePipeline::from_instances(vec![first, second]).with_limits(Limits {
            budget: Duration::from_millis(100),
            breaker_failures: 2,
            ..Limits::default()
        });
        pipeline.breakers().probe_failed("first");
        pipeline.breakers().probe_failed("second");
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
    async fn an_oversized_request_is_denied_and_not_sent_whatever_on_error_says() {
        for on_error in [OnError::Deny, OnError::Allow] {
            let module = StubModule::allowing();
            let pipeline = ModulePipeline::from_instances(vec![instance(
                "screening",
                &[HOOK],
                on_error,
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
            assert!(
                matches!(
                    refused,
                    Err(GuardianError::ModuleDenied { ref module, ref reason_code, .. })
                        if module == "screening" && reason_code == "request_too_large"
                ),
                "{on_error:?}: {refused:?}"
            );
            assert_eq!(module.calls(), 0);
        }
    }

    #[tokio::test]
    async fn a_generic_instance_refuses_without_naming_itself() {
        let mut generic = instance("screening", &[HOOK], OnError::Allow, StubModule::allowing());
        generic.config.client_reason = crate::modules::catalog::ClientReason::Generic;
        let pipeline = ModulePipeline::from_instances(vec![generic]).with_limits(Limits {
            request_max_bytes: 64,
            ..Limits::default()
        });
        let oversized = pipeline
            .run(HOOK, "0xacc", || {
                Ok(HookRequest {
                    submitter: "0x".repeat(64),
                    ..Default::default()
                })
            })
            .await;
        assert!(
            matches!(oversized, Err(GuardianError::ModuleDeniedGeneric)),
            "{oversized:?}"
        );
        let unbuildable = pipeline
            .run(HOOK, "0xacc", || Err("view failed".into()))
            .await;
        assert!(
            matches!(
                unbuildable,
                Err(GuardianError::ModuleUnavailable { module: None })
            ),
            "{unbuildable:?}"
        );
    }

    #[tokio::test]
    async fn refused_requests_never_open_the_circuit() {
        let module = StubModule::allowing();
        let pipeline = ModulePipeline::from_instances(vec![instance(
            "screening",
            &[HOOK],
            OnError::Deny,
            module.clone(),
        )])
        .with_limits(Limits {
            request_max_bytes: 64,
            breaker_failures: 1,
            ..Limits::default()
        });
        for _ in 0..3 {
            let _ = pipeline
                .run(HOOK, "0xacc", || {
                    Ok(HookRequest {
                        submitter: "0x".repeat(64),
                        ..Default::default()
                    })
                })
                .await;
            let _ = pipeline
                .run(HOOK, "0xacc", || Err("view failed".into()))
                .await;
        }
        assert_eq!(
            pipeline.breakers().state("screening"),
            CircuitState::Closed,
            "a requester's refused requests say nothing about whether the module is up"
        );
        run(&pipeline)
            .await
            .expect("the next request reaches the module");
        assert_eq!(module.calls(), 1);
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
}
