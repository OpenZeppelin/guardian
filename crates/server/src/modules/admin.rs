//! Operator management of module parameters: global params, account layers, assignments.

use serde::Serialize;

use super::catalog::{ClientReason, Disclosure, Scope, contains_secret_reference};
use super::pipeline::{ModulePipeline, Rows};
use super::registry::{Instance, Unhealthy, validate_params};
use super::store::{Assigned, Change, HistoryEntry, InstanceRow, ParamsKind, WriteError};
use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::dashboard::AuthenticatedOperator;
use crate::error::{GuardianError, Result};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ModuleInstanceView {
    pub module_id: String,
    pub kind: String,
    #[schema(value_type = String)]
    pub scope: &'static str,
    pub hooks: Vec<String>,
    pub enabled: bool,
    pub catalog_enabled: bool,
    pub enabled_drift: bool,
    pub operator_can_disable: bool,
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
    pub params_drift: bool,
    pub file_managed_params: bool,
    /// `name version` from `describe` at startup; absent when the startup checks failed.
    pub build: Option<String>,
    pub disclosure: Disclosure,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub client_reason: ClientReason,
    pub dry_run: bool,
    /// The instance's circuit breaker on the replica that answered: `closed`, `open` or
    /// `half_open`.
    #[schema(value_type = String)]
    pub circuit: &'static str,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ModuleCatalogView {
    pub catalog_hash: String,
    pub params_version: u64,
    pub modules: Vec<ModuleInstanceView>,
    /// Stored instances the catalog no longer declares: kept, never evaluated, active again if
    /// the catalog brings the id back.
    pub undeclared: Vec<UndeclaredInstanceView>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct UndeclaredInstanceView {
    pub module_id: String,
    pub enabled: bool,
    #[schema(value_type = Object)]
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AccountModuleEntry {
    pub module_id: String,
    #[schema(value_type = String)]
    pub scope: &'static str,
    pub enabled: bool,
    /// A stored entry naming an instance the catalog no longer declares; skipped at evaluation.
    pub orphaned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub global_params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub account_layer: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AccountModulesView {
    pub account_id: String,
    /// The account's params version: moves with every write to its layers or assignment, and
    /// is recorded with each delta the account's modules approve.
    pub account_params_version: u64,
    pub modules: Vec<AccountModuleEntry>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ParamsHistoryView {
    pub module_id: String,
    pub entries: Vec<HistoryEntry>,
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::All => "all",
        Scope::Default => "default",
        Scope::Assigned => "assigned",
    }
}

fn pipeline(state: &AppState) -> Result<&ModulePipeline> {
    state
        .modules
        .as_deref()
        .ok_or_else(|| GuardianError::ModuleWriteRefused("no policy modules are configured".into()))
}

fn instance<'a>(pipeline: &'a ModulePipeline, module_id: &str) -> Result<&'a Instance> {
    pipeline
        .instance(module_id)
        .ok_or_else(|| GuardianError::ModuleNotFound {
            module: module_id.to_string(),
        })
}

const MAX_REASON_CHARS: usize = 512;

fn require_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        return Err(GuardianError::ModuleWriteRefused(
            "reason is required".into(),
        ));
    }
    if reason.chars().count() > MAX_REASON_CHARS {
        return Err(GuardianError::ModuleWriteRefused(format!(
            "reason exceeds {MAX_REASON_CHARS} character limit"
        )));
    }
    Ok(())
}

/// Validates operator-supplied params and returns what to store: the module's normalized form
/// when it sent one.
async fn validated(
    pipeline: &ModulePipeline,
    instance: &Instance,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    if !params.is_object() {
        return Err(GuardianError::ModuleWriteRefused(
            "params must be a JSON object".into(),
        ));
    }
    pipeline.limits().check_params_size(params).map_err(|e| {
        GuardianError::ModuleWriteRefused(format!("module {}: {e}", instance.config.id))
    })?;
    if contains_secret_reference(params) {
        return Err(GuardianError::ModuleWriteRefused(
            "secret references cannot be set through the operator API".into(),
        ));
    }
    let stored = validate_params(&instance.module, params, instance.config.timeout)
        .await
        .map_err(|e| match e {
            Unhealthy::InvalidParams(_) => {
                GuardianError::ModuleWriteRefused(format!("module {}: {e}", instance.config.id))
            }
            Unhealthy::Unreachable(_) | Unhealthy::Incompatible(_) => {
                tracing::warn!(module = %instance.config.id, error = %e, "Policy module could not validate operator params");
                GuardianError::ModuleUnavailable {
                    module: Some(instance.config.id.clone()),
                }
            }
        })?;
    pipeline.storable_normalization(&stored).map_err(|e| {
        GuardianError::ModuleWriteRefused(format!("module {}: {e}", instance.config.id))
    })?;
    Ok(stored)
}

/// The key the account's module rows use: its id lowercased, as evaluation looks them up.
/// Registration stores the id as the client sent it, so the account is looked up as given and
/// then lowercased.
async fn existing_account(state: &AppState, account_id: &str) -> Result<String> {
    let key = account_id.to_ascii_lowercase();
    for candidate in [account_id, key.as_str()] {
        match state.metadata.get(candidate).await {
            Ok(Some(_)) => return Ok(key.clone()),
            Ok(None) => continue,
            Err(e) => {
                return Err(GuardianError::StorageError(format!(
                    "account lookup failed: {e}"
                )));
            }
        }
    }
    Err(GuardianError::AccountNotFound(account_id.to_string()))
}

fn audit(
    state: &AppState,
    operator: &AuthenticatedOperator,
    kind: &'static str,
    account_id: Option<&str>,
    payload: serde_json::Value,
    client_ip: Option<String>,
) {
    state.auditor.record(AuditEvent {
        operator_identity: operator.operator_id.clone(),
        action_kind: kind,
        target_account_id: account_id.map(str::to_string),
        payload,
        outcome: AuditOutcome::Success,
        error_code: None,
        client_ip,
    });
}

fn change<'a>(
    state: &AppState,
    operator: &'a AuthenticatedOperator,
    reason: &'a str,
) -> Change<'a> {
    Change {
        operator: &operator.operator_id,
        reason,
        at: state.clock.now_rfc3339(),
        expected_version: None,
    }
}

/// An operator's params write: its reason and, optionally, the params version it was made
/// against (the instance params version for a global write, the account's for a layer or an
/// assignment), so a write from a stale read is refused instead of dropping a newer change.
#[derive(Debug, Clone, Copy)]
pub struct Write<'a> {
    pub reason: &'a str,
    pub expected_version: Option<u64>,
}

impl<'a> From<&'a str> for Write<'a> {
    fn from(reason: &'a str) -> Self {
        Write {
            reason,
            expected_version: None,
        }
    }
}

impl<'a> From<&'a String> for Write<'a> {
    fn from(reason: &'a String) -> Self {
        Write::from(reason.as_str())
    }
}

fn change_for<'a>(
    state: &AppState,
    operator: &'a AuthenticatedOperator,
    write: Write<'a>,
) -> Change<'a> {
    Change {
        expected_version: write.expected_version,
        ..change(state, operator, write.reason)
    }
}

fn write_error(error: WriteError) -> GuardianError {
    match error {
        WriteError::Stale { .. } => GuardianError::ModuleParamsStale,
        WriteError::Storage(message) => GuardianError::StorageError(message),
    }
}

async fn instance_view(
    pipeline: &ModulePipeline,
    instance: &Instance,
) -> Result<ModuleInstanceView> {
    let stored = stored_instances(pipeline).await?;
    Ok(view_in(pipeline, instance, &rows_of(&stored), &stored))
}

fn rows_of(stored: &[InstanceRow]) -> Rows {
    std::sync::Arc::new(
        stored
            .iter()
            .map(|row| {
                (
                    row.module_id.clone(),
                    (row.global_params.clone(), row.enabled),
                )
            })
            .collect(),
    )
}

async fn stored_instances(pipeline: &ModulePipeline) -> Result<Vec<InstanceRow>> {
    pipeline
        .store()
        .instances()
        .await
        .map_err(GuardianError::StorageError)
}

/// One instance as `rows` and `stored` show it, so every instance in a listing comes from the
/// same reads.
fn view_in(
    pipeline: &ModulePipeline,
    instance: &Instance,
    rows: &Rows,
    stored: &[InstanceRow],
) -> ModuleInstanceView {
    let params = ModulePipeline::global_params_in(rows, instance);
    let enabled = ModulePipeline::enabled_in(rows, instance);
    let seeded = stored
        .iter()
        .find(|row| row.module_id == instance.config.id)
        .map(|row| &row.seeded_params);
    ModuleInstanceView {
        module_id: instance.config.id.clone(),
        kind: instance.config.kind.clone(),
        scope: scope_name(instance.config.scope),
        hooks: instance
            .config
            .hooks
            .iter()
            .map(|hook| hook.as_str().to_string())
            .collect(),
        enabled,
        catalog_enabled: instance.config.catalog_enabled,
        enabled_drift: enabled != instance.config.catalog_enabled,
        operator_can_disable: instance.config.operator_can_disable,
        params_drift: !instance.config.file_managed_params
            && seeded.is_some_and(|seeded| *seeded != instance.config.params),
        params,
        file_managed_params: instance.config.file_managed_params,
        build: instance.build.clone(),
        disclosure: instance.config.disclosure,
        description: instance.config.description.clone(),
        client_reason: instance.config.client_reason,
        dry_run: instance.config.dry_run,
        circuit: pipeline.breakers().state(&instance.config.id).as_str(),
    }
}

/// The catalog from one read of the stored instances, with `params_version` read before it,
/// so a write landing between the two reads pairs newer params with the older version: a
/// fenced write based on the listing is then refused as stale, never applied over the newer
/// params.
pub async fn catalog_view(state: &AppState) -> Result<ModuleCatalogView> {
    let pipeline = pipeline(state)?;
    let params_version = pipeline
        .store()
        .instance_version()
        .await
        .map_err(GuardianError::StorageError)?;
    let stored = stored_instances(pipeline).await?;
    let rows = rows_of(&stored);
    let modules = pipeline
        .instances()
        .iter()
        .map(|instance| view_in(pipeline, instance, &rows, &stored))
        .collect();
    let undeclared = stored
        .into_iter()
        .filter(|row| pipeline.instance(&row.module_id).is_none())
        .map(|row| UndeclaredInstanceView {
            module_id: row.module_id,
            enabled: row.enabled,
            params: row.global_params,
        })
        .collect();
    Ok(ModuleCatalogView {
        catalog_hash: pipeline.catalog_hash.clone(),
        params_version,
        modules,
        undeclared,
    })
}

pub async fn account_view(state: &AppState, account_id: &str) -> Result<AccountModulesView> {
    let account_id = existing_account(state, account_id).await?;
    view_of(state, &account_id).await
}

/// The modules of the account whose module rows are keyed `account_id`.
async fn view_of(state: &AppState, account_id: &str) -> Result<AccountModulesView> {
    let pipeline = pipeline(state)?;
    let super::pipeline::Snapshot {
        account_params_version,
        rows: instance_rows,
        account_rows: rows,
        ..
    } = pipeline.snapshot(account_id).await?;
    let mut modules = Vec::new();
    for instance in pipeline
        .instances()
        .iter()
        .filter(|instance| instance.config.scope == Scope::All)
    {
        modules.push(AccountModuleEntry {
            module_id: instance.config.id.clone(),
            scope: "all",
            enabled: ModulePipeline::enabled_in(&instance_rows, instance),
            orphaned: false,
            global_params: Some(ModulePipeline::global_params_in(&instance_rows, instance)),
            account_layer: rows
                .iter()
                .find(|row| row.kind == ParamsKind::Layer && row.module_id == instance.config.id)
                .map(|row| row.params.clone()),
            params: None,
        });
    }
    for row in rows.iter().filter(|row| {
        row.kind == ParamsKind::Layer && !pipeline.row_applies(&row.module_id, row.kind)
    }) {
        modules.push(AccountModuleEntry {
            module_id: row.module_id.clone(),
            scope: pipeline
                .instance(&row.module_id)
                .map_or("all", |instance| scope_name(instance.config.scope)),
            enabled: false,
            orphaned: true,
            global_params: None,
            account_layer: Some(row.params.clone()),
            params: None,
        });
    }
    for row in rows
        .into_iter()
        .filter(|row| row.kind == ParamsKind::Assignment)
    {
        let instance = pipeline.instance(&row.module_id);
        modules.push(AccountModuleEntry {
            scope: instance.map_or("assigned", |instance| scope_name(instance.config.scope)),
            enabled: match instance {
                Some(instance) => ModulePipeline::enabled_in(&instance_rows, instance),
                None => false,
            },
            orphaned: !pipeline.row_applies(&row.module_id, ParamsKind::Assignment),
            module_id: row.module_id,
            global_params: None,
            account_layer: None,
            params: Some(row.params),
        });
    }
    Ok(AccountModulesView {
        account_id: account_id.to_string(),
        account_params_version,
        modules,
    })
}

const HISTORY_PAGE_DEFAULT: usize = 100;
const HISTORY_PAGE_MAX: usize = 1_000;

/// The instance's params changes, global and per account, newest first: those whose `id` is
/// below `before` when set, at most `limit` (default 100, at most 1,000).
pub async fn history(
    state: &AppState,
    module_id: &str,
    before: Option<u64>,
    limit: Option<usize>,
) -> Result<ParamsHistoryView> {
    let pipeline = pipeline(state)?;
    instance(pipeline, module_id)?;
    let limit = limit.unwrap_or(HISTORY_PAGE_DEFAULT);
    if limit == 0 || limit > HISTORY_PAGE_MAX {
        return Err(GuardianError::ModuleWriteRefused(format!(
            "limit must be between 1 and {HISTORY_PAGE_MAX}"
        )));
    }
    let entries = pipeline
        .store()
        .history(module_id, before, limit)
        .await
        .map_err(GuardianError::StorageError)?;
    Ok(ParamsHistoryView {
        module_id: module_id.to_string(),
        entries,
    })
}

/// Switches an instance on for every account, after re-running its startup checks.
/// Idempotent: enabling an enabled instance changes nothing but is still audited.
pub async fn enable(
    state: &AppState,
    operator: &AuthenticatedOperator,
    module_id: &str,
    reason: &str,
    client_ip: Option<String>,
) -> Result<ModuleInstanceView> {
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let instance = instance(pipeline, module_id)?;
    let mut committed = None;
    if !pipeline.enabled(instance).await? {
        let params = pipeline.global_params(instance).await?;
        let checked = super::registry::check(&instance.config, &instance.module, &params)
            .await
            .map_err(|error| {
                tracing::warn!(module = %module_id, %error, "Module failed its checks; not enabled");
                GuardianError::ModuleUnavailable {
                    module: Some(module_id.to_string()),
                }
            })?;
        let storable = pipeline
            .storable_normalization(&checked.params)
            .inspect_err(|error| {
                tracing::warn!(module = %module_id, %error, "Module's normalized params break the params rules; kept the stored params");
            })
            .is_ok();
        if checked.params != params && !instance.config.file_managed_params && storable {
            let migrator = format!(
                "module:{}",
                instance.build.as_deref().unwrap_or(&instance.config.kind)
            );
            let migrated = pipeline
                .store()
                .set_global_if(
                    module_id,
                    &params,
                    checked.params,
                    &Change {
                        operator: &migrator,
                        reason: "params migrated by the module",
                        at: state.clock.now_rfc3339(),
                        expected_version: None,
                    },
                )
                .await
                .map_err(GuardianError::StorageError)?;
            if migrated.is_none() {
                tracing::warn!(module = %module_id, "Global params changed while the module migrated them; kept the newer value");
            }
        }
        committed = Some(
            pipeline
                .store()
                .set_enabled(module_id, true, &change(state, operator, reason))
                .await
                .map_err(GuardianError::StorageError)?,
        );
        super::metrics::enabled(module_id, true);
    }
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "params_version": committed });
    audit(
        state,
        operator,
        kinds::MODULES_ENABLE,
        None,
        payload,
        client_ip,
    );
    instance_view(pipeline, instance).await
}

/// Switches an instance off for every account, only if its catalog entry allows it.
pub async fn disable(
    state: &AppState,
    operator: &AuthenticatedOperator,
    module_id: &str,
    reason: &str,
    client_ip: Option<String>,
) -> Result<ModuleInstanceView> {
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let instance = instance(pipeline, module_id)?;
    if !instance.config.operator_can_disable {
        return Err(GuardianError::ModuleNotDisableable {
            module: module_id.to_string(),
        });
    }
    let mut committed = None;
    if pipeline.enabled(instance).await? {
        committed = Some(
            pipeline
                .store()
                .set_enabled(module_id, false, &change(state, operator, reason))
                .await
                .map_err(GuardianError::StorageError)?,
        );
        super::metrics::enabled(module_id, false);
    }
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "params_version": committed });
    audit(
        state,
        operator,
        kinds::MODULES_DISABLE,
        None,
        payload,
        client_ip,
    );
    instance_view(pipeline, instance).await
}

pub async fn set_global<'a>(
    state: &AppState,
    operator: &'a AuthenticatedOperator,
    module_id: &str,
    params: serde_json::Value,
    write: impl Into<Write<'a>>,
    client_ip: Option<String>,
) -> Result<ModuleInstanceView> {
    let write = write.into();
    let reason = write.reason;
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let instance = instance(pipeline, module_id)?;
    if instance.config.file_managed_params {
        return Err(GuardianError::ModuleWriteRefused(format!(
            "module {module_id}: params with secret references are managed in the catalog file"
        )));
    }
    let params = validated(pipeline, instance, &params).await?;
    let committed = pipeline
        .store()
        .set_global(module_id, params, &change_for(state, operator, write))
        .await
        .map_err(write_error)?;
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "params_version": committed });
    audit(
        state,
        operator,
        kinds::MODULES_PARAMS_GLOBAL,
        None,
        payload,
        client_ip,
    );
    instance_view(pipeline, instance).await
}

pub async fn set_layer<'a>(
    state: &AppState,
    operator: &'a AuthenticatedOperator,
    account_id: &str,
    module_id: &str,
    params: Option<serde_json::Value>,
    write: impl Into<Write<'a>>,
    client_ip: Option<String>,
) -> Result<AccountModulesView> {
    let write = write.into();
    let reason = write.reason;
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let account_id = existing_account(state, account_id).await?;
    let instance = instance(pipeline, module_id)?;
    if params.is_some() && instance.config.scope != Scope::All {
        return Err(GuardianError::ModuleWriteRefused(format!(
            "module {module_id}: account layers apply only to scope = \"all\" instances; assign it instead"
        )));
    }
    let params = match params {
        Some(params) => Some(validated(pipeline, instance, &params).await?),
        None => None,
    };
    let (current, read_version) = account_rows(pipeline, &account_id).await?;
    if params.is_some()
        && current
            .iter()
            .any(|row| row.kind == ParamsKind::Assignment && row.module_id == module_id)
    {
        return Err(GuardianError::ModuleWriteRefused(format!(
            "module {module_id}: the account still has it assigned from when it applied only to assigned accounts; remove it from the account's assignment first"
        )));
    }
    let mut rows = current.clone();
    rows.retain(|row| !(row.kind == ParamsKind::Layer && row.module_id == module_id));
    if let Some(params) = &params {
        rows.push(proposed_row(
            &account_id,
            module_id,
            ParamsKind::Layer,
            params,
        ));
    }
    pipeline.check_budget(&current, &rows)?;
    let committed = pipeline
        .store()
        .set_layer(
            &account_id,
            module_id,
            params,
            &account_change(state, operator, write, read_version)?,
        )
        .await
        .map_err(write_error)?;
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "account_params_version": committed });
    audit(
        state,
        operator,
        kinds::MODULES_PARAMS_ACCOUNT,
        Some(&account_id),
        payload,
        client_ip,
    );
    view_of(state, &account_id).await
}

/// The account's rows and the account params version they were read at, in one read.
async fn account_rows(
    pipeline: &ModulePipeline,
    account_id: &str,
) -> Result<(Vec<super::store::AccountParamsRow>, u64)> {
    let snapshot = pipeline
        .store()
        .snapshot(account_id, None)
        .await
        .map_err(GuardianError::StorageError)?;
    Ok((snapshot.account_params, snapshot.account_version))
}

/// The change for a layer or assignment write. The write is budget-checked against the rows
/// read at `read_version`, so it commits only while the account is still at that version. Two
/// writes from the same rows can then never both commit and together exceed the budget. An
/// operator's `expected_version` other than `read_version` is stale (or names a version the
/// rows have not reached), and the write is refused before it is attempted.
fn account_change<'a>(
    state: &AppState,
    operator: &'a AuthenticatedOperator,
    write: Write<'a>,
    read_version: u64,
) -> Result<Change<'a>> {
    if write
        .expected_version
        .is_some_and(|expected| expected != read_version)
    {
        return Err(GuardianError::ModuleParamsStale);
    }
    Ok(Change {
        expected_version: Some(read_version),
        ..change(state, operator, write.reason)
    })
}

/// Whether the rows hold exactly this assignment.
fn is_assigned(
    rows: &[super::store::AccountParamsRow],
    module_id: &str,
    params: &serde_json::Value,
) -> bool {
    rows.iter().any(|row| {
        row.kind == ParamsKind::Assignment && row.module_id == module_id && &row.params == params
    })
}

/// A row as a pending write would store it, for the budget check before the write.
fn proposed_row(
    account_id: &str,
    module_id: &str,
    kind: ParamsKind,
    params: &serde_json::Value,
) -> super::store::AccountParamsRow {
    super::store::AccountParamsRow {
        account_id: account_id.to_string(),
        module_id: module_id.to_string(),
        kind,
        position: 0,
        params: params.clone(),
        updated_at: String::new(),
        updated_by: String::new(),
    }
}

pub async fn set_assignment<'a>(
    state: &AppState,
    operator: &'a AuthenticatedOperator,
    account_id: &str,
    modules: Vec<Assigned>,
    write: impl Into<Write<'a>>,
    client_ip: Option<String>,
) -> Result<AccountModulesView> {
    let write = write.into();
    let reason = write.reason;
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let account_id = existing_account(state, account_id).await?;
    let (earlier, _) = account_rows(pipeline, &account_id).await?;
    let mut to_store = Vec::with_capacity(modules.len());
    let mut checked = Vec::with_capacity(modules.len());
    for (index, assigned) in modules.iter().enumerate() {
        let instance = instance(pipeline, &assigned.module_id)?;
        if instance.config.scope == Scope::All {
            return Err(GuardianError::ModuleWriteRefused(format!(
                "module {}: scope = \"all\" applies to every account; set an account layer instead",
                assigned.module_id
            )));
        }
        if modules[..index]
            .iter()
            .any(|earlier| earlier.module_id == assigned.module_id)
        {
            return Err(GuardianError::ModuleWriteRefused(format!(
                "module {} listed twice",
                assigned.module_id
            )));
        }
        let kept = is_assigned(&earlier, &assigned.module_id, &assigned.params);
        let params = if kept {
            assigned.params.clone()
        } else {
            validated(pipeline, instance, &assigned.params).await?
        };
        checked.push(!kept);
        to_store.push(Assigned {
            module_id: assigned.module_id.clone(),
            params,
        });
    }
    // Validation calls the modules and can take their whole timeouts. The rows the write is
    // budgeted and fenced against are read after it, so another write to the account meanwhile
    // does not make this one stale; params kept unchecked because they were stored are checked
    // now if that write replaced them.
    let (current, read_version) = account_rows(pipeline, &account_id).await?;
    for (assigned, checked) in to_store.iter_mut().zip(checked) {
        if !checked && !is_assigned(&current, &assigned.module_id, &assigned.params) {
            let instance = instance(pipeline, &assigned.module_id)?;
            assigned.params = validated(pipeline, instance, &assigned.params).await?;
        }
    }
    if let Some(layer) = current.iter().find(|row| {
        row.kind == ParamsKind::Layer
            && to_store
                .iter()
                .any(|assigned| assigned.module_id == row.module_id)
    }) {
        return Err(GuardianError::ModuleWriteRefused(format!(
            "module {}: the account still has a layer on it from when it applied to every account; delete the layer first",
            layer.module_id
        )));
    }
    // Rows naming an instance the catalog no longer declares are kept as they are: they apply
    // again if the instance comes back, and the operator cannot list them in the request.
    to_store.extend(
        current
            .iter()
            .filter(|row| {
                row.kind == ParamsKind::Assignment && pipeline.instance(&row.module_id).is_none()
            })
            .map(|row| Assigned {
                module_id: row.module_id.clone(),
                params: row.params.clone(),
            }),
    );
    let mut rows = current.clone();
    rows.retain(|row| row.kind == ParamsKind::Layer);
    rows.extend(to_store.iter().map(|assigned| {
        proposed_row(
            &account_id,
            &assigned.module_id,
            ParamsKind::Assignment,
            &assigned.params,
        )
    }));
    pipeline.check_budget(&current, &rows)?;
    let assigned: Vec<String> = to_store
        .iter()
        .map(|assigned| assigned.module_id.clone())
        .collect();
    let committed = pipeline
        .store()
        .set_assignment(
            &account_id,
            to_store,
            &account_change(state, operator, write, read_version)?,
        )
        .await
        .map_err(write_error)?;
    let payload = serde_json::json!({ "reason": reason, "modules": assigned, "account_params_version": committed });
    audit(
        state,
        operator,
        kinds::MODULES_ASSIGNMENT,
        Some(&account_id),
        payload,
        client_ip,
    );
    view_of(state, &account_id).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;

    use guardian_module_sdk::policies::RecipientPolicy;
    use serde_json::json;

    use super::*;
    use crate::metadata::AccountMetadata;
    use crate::metadata::auth::Auth;
    use crate::modules::catalog::{HookName, OnError};
    use crate::modules::testing::{instance, instance_with_params, pipeline};
    use crate::testing::helpers::create_test_app_state_with_mocks;
    use crate::testing::mocks::{MockMetadataStore, MockNetworkClient, MockStorageBackend};

    const ACCOUNT: &str = "0xacc";

    fn operator() -> AuthenticatedOperator {
        AuthenticatedOperator {
            operator_id: "0xop".into(),
            commitment: "0xop".into(),
            effective_permissions: Arc::new(BTreeSet::new()),
        }
    }

    fn state() -> AppState {
        let metadata = MockMetadataStore::new().with_row(AccountMetadata {
            account_id: ACCOUNT.into(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xs1".into()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-10-15T00:00:00Z".into(),
            updated_at: "2026-10-15T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        });
        let mut state = create_test_app_state_with_mocks(
            Arc::new(MockStorageBackend::new()),
            Arc::new(MockNetworkClient::new()),
            Arc::new(metadata),
        );
        let hooks = [HookName::BeforeTransactionApproved];
        let mut optional = instance("optional", &hooks, OnError::Deny, Arc::new(RecipientPolicy));
        optional.config.scope = Scope::Assigned;
        let mut secret = instance("vendor", &hooks, OnError::Deny, Arc::new(RecipientPolicy));
        secret.config.file_managed_params = true;
        secret.config.operator_can_disable = false;
        state.modules = Some(pipeline(vec![
            instance_with_params(
                "blocklist",
                &hooks,
                Arc::new(RecipientPolicy),
                json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
            ),
            optional,
            secret,
        ]));
        state
    }

    /// A catalog that changes an instance's `scope` leaves rows evaluation no longer reads: they
    /// show as orphaned in the account view and count in the orphaned-rows report.
    #[tokio::test]
    async fn rows_left_by_a_scope_change_are_reported_as_orphaned() {
        let store: Arc<dyn super::super::store::ModuleStore> =
            Arc::new(super::super::store::FsModuleStore::in_memory());
        let hooks = [HookName::BeforeTransactionApproved];
        let mut state = state();
        state.modules = Some(Arc::new(ModulePipeline::from_instances_with_store(
            vec![instance(
                "caps",
                &hooks,
                OnError::Deny,
                Arc::new(RecipientPolicy),
            )],
            store.clone(),
        )));
        set_layer(
            &state,
            &operator(),
            ACCOUNT,
            "caps",
            Some(json!({ "denylist": ["0x000000000000000000000000000aaa"] })),
            "OP-1",
            None,
        )
        .await
        .unwrap();

        let mut now_assigned = instance("caps", &hooks, OnError::Deny, Arc::new(RecipientPolicy));
        now_assigned.config.scope = Scope::Assigned;
        let pipeline = Arc::new(ModulePipeline::from_instances_with_store(
            vec![now_assigned],
            store,
        ));
        state.modules = Some(pipeline.clone());
        let view = account_view(&state, ACCOUNT).await.unwrap();
        let layer = view
            .modules
            .iter()
            .find(|entry| entry.module_id == "caps")
            .expect("the left-over layer is shown");
        assert!(layer.orphaned, "{layer:?}");
        assert_eq!(layer.scope, "assigned");
        assert_eq!(pipeline.report_stored_assignments().await.unwrap().1, 1);
    }

    /// One module being down must not freeze every assignment that keeps it: an entry stored
    /// unchanged is not sent to the module again, and a change it cannot validate is refused
    /// as unavailable, not as invalid params.
    #[tokio::test]
    async fn an_assignment_keeping_a_down_module_unchanged_can_still_be_edited() {
        let vendor = crate::modules::testing::Outage::new(false);
        let mut down = instance(
            "vendor-assigned",
            &[HookName::BeforeTransactionApproved],
            OnError::Deny,
            vendor.clone(),
        );
        down.config.scope = Scope::Assigned;
        let mut optional = instance(
            "optional",
            &[HookName::BeforeTransactionApproved],
            OnError::Deny,
            Arc::new(RecipientPolicy),
        );
        optional.config.scope = Scope::Assigned;
        let mut state = state();
        state.modules = Some(pipeline(vec![down, optional]));
        let operator = operator();
        let kept = Assigned {
            module_id: "vendor-assigned".into(),
            params: json!({ "tier": 1 }),
        };
        set_assignment(&state, &operator, ACCOUNT, vec![kept.clone()], "OP-1", None)
            .await
            .unwrap();
        vendor.set_down(true);

        let added = Assigned {
            module_id: "optional".into(),
            params: json!({}),
        };
        let edited = set_assignment(
            &state,
            &operator,
            ACCOUNT,
            vec![kept.clone(), added],
            "OP-2",
            None,
        )
        .await;
        assert!(edited.is_ok(), "{edited:?}");

        let changed = Assigned {
            module_id: "vendor-assigned".into(),
            params: json!({ "tier": 2 }),
        };
        let refused = set_assignment(&state, &operator, ACCOUNT, vec![changed], "OP-3", None).await;
        assert!(
            matches!(
                refused,
                Err(GuardianError::ModuleUnavailable { module: Some(ref module) }) if module == "vendor-assigned"
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn operator_writes_store_the_shape_the_module_normalized() {
        let mut stub = crate::modules::testing::ProtocolStub::new();
        stub.normalized = Some(json!({ "new_name": 5 }));
        let mut state = state();
        state.modules = Some(pipeline(vec![instance(
            "migrating",
            &[HookName::BeforeTransactionApproved],
            OnError::Deny,
            Arc::new(stub),
        )]));
        let view = set_global(
            &state,
            &operator(),
            "migrating",
            json!({ "old_name": 5 }),
            "OP-93",
            None,
        )
        .await
        .unwrap();
        assert_eq!(view.params, json!({ "new_name": 5 }));
        let view = set_layer(
            &state,
            &operator(),
            ACCOUNT,
            "migrating",
            Some(json!({ "old_name": 1 })),
            "OP-94",
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            view.modules[0].account_layer,
            Some(json!({ "new_name": 5 }))
        );
    }

    /// `state()`'s instances (50 ms each; `blocklist` and `vendor` on every account) under a
    /// 120 ms budget: 100 ms before any account write.
    fn tight_budget(state: &mut AppState) {
        let instances = state.modules.as_ref().unwrap().instances().to_vec();
        state.modules = Some(Arc::new(
            ModulePipeline::from_instances(instances).with_limits(crate::modules::limits::Limits {
                budget: std::time::Duration::from_millis(120),
                ..Default::default()
            }),
        ));
    }

    #[tokio::test]
    async fn views_report_real_scopes_orphaned_rows_and_undeclared_instances() {
        let mut state = state();
        let store = state.modules.as_ref().unwrap().store_handle();
        let mut instances = state.modules.as_ref().unwrap().instances().to_vec();
        let mut screening = instance(
            "screening",
            &[HookName::BeforeTransactionApproved],
            OnError::Deny,
            Arc::new(RecipientPolicy),
        );
        screening.config.scope = Scope::Default;
        instances.push(screening);
        store
            .seed_instance(
                "retired",
                true,
                &json!({ "old": true }),
                &json!({ "old": true }),
                "t0",
            )
            .await
            .unwrap();
        store
            .set_assignment(
                ACCOUNT,
                vec![
                    Assigned {
                        module_id: "screening".into(),
                        params: json!({}),
                    },
                    Assigned {
                        module_id: "retired".into(),
                        params: json!({}),
                    },
                ],
                &Change {
                    operator: "0xop",
                    reason: "setup",
                    at: "t1".into(),
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        state.modules = Some(Arc::new(ModulePipeline::from_instances_with_store(
            instances, store,
        )));

        let account = account_view(&state, ACCOUNT).await.unwrap();
        let entry = |id: &str| {
            account
                .modules
                .iter()
                .find(|entry| entry.module_id == id)
                .unwrap()
                .clone()
        };
        assert_eq!(entry("screening").scope, "default");
        assert!(!entry("screening").orphaned);
        assert!(entry("retired").orphaned);
        assert!(!entry("retired").enabled);

        let catalog = catalog_view(&state).await.unwrap();
        assert_eq!(catalog.undeclared.len(), 1);
        assert_eq!(catalog.undeclared[0].module_id, "retired");

        let after = set_assignment(&state, &operator(), ACCOUNT, Vec::new(), "OP-9", None)
            .await
            .unwrap();
        let kept: Vec<_> = after
            .modules
            .iter()
            .filter(|entry| entry.orphaned)
            .map(|entry| entry.module_id.as_str())
            .collect();
        assert_eq!(kept, ["retired"], "an assignment write keeps orphaned rows");
    }

    #[tokio::test]
    async fn the_catalog_view_shows_each_instance_circuit_on_this_replica() {
        let state = state();
        let pipeline = state.modules.as_ref().unwrap();
        for _ in 0..crate::modules::limits::Limits::default().breaker_failures {
            pipeline.breakers().probe_failed("blocklist");
        }
        let view = catalog_view(&state).await.unwrap();
        let circuit = |id: &str| {
            view.modules
                .iter()
                .find(|module| module.module_id == id)
                .unwrap()
                .circuit
        };
        assert_eq!(circuit("blocklist"), "open");
        assert_eq!(circuit("optional"), "closed");
    }

    #[tokio::test]
    async fn an_account_write_that_could_exceed_the_budget_is_refused_and_stores_nothing() {
        let mut state = state();
        tight_budget(&mut state);
        let layer = set_layer(
            &state,
            &operator(),
            ACCOUNT,
            "blocklist",
            Some(json!({ "denylist": ["0x000000000000000000000000000bbb"] })),
            "OP-1",
            None,
        )
        .await;
        assert!(
            matches!(&layer, Err(GuardianError::ModuleWriteRefused(message)) if message.contains("budget")),
            "{layer:?}"
        );
        let assignment = set_assignment(
            &state,
            &operator(),
            ACCOUNT,
            vec![Assigned {
                module_id: "optional".into(),
                params: json!({}),
            }],
            "OP-2",
            None,
        )
        .await;
        assert!(matches!(
            assignment,
            Err(GuardianError::ModuleWriteRefused(_))
        ));
        let pipeline = state.modules.as_ref().unwrap();
        assert!(
            pipeline
                .store()
                .account_params(ACCOUNT)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn accounts_stored_over_the_budget_are_reported_at_startup() {
        let mut state = state();
        set_assignment(
            &state,
            &operator(),
            ACCOUNT,
            vec![Assigned {
                module_id: "optional".into(),
                params: json!({}),
            }],
            "OP-3",
            None,
        )
        .await
        .unwrap();
        let store = state.modules.as_ref().unwrap().store_handle();
        let instances = state.modules.as_ref().unwrap().instances().to_vec();
        state.modules = Some(Arc::new(
            ModulePipeline::from_instances_with_store(instances, store).with_limits(
                crate::modules::limits::Limits {
                    budget: std::time::Duration::from_millis(120),
                    ..Default::default()
                },
            ),
        ));
        assert_eq!(
            state
                .modules
                .as_ref()
                .unwrap()
                .report_stored_assignments()
                .await
                .unwrap(),
            (1, 0)
        );

        set_assignment(&state, &operator(), ACCOUNT, Vec::new(), "OP-2", None)
            .await
            .expect("removing the assignment brings the account back under the budget");
        let modules = state.modules.as_ref().unwrap();
        assert_eq!(modules.assignment_report(), (1, 0), "not recounted yet");
        modules.refresh_assignment_report().await;
        assert_eq!(
            modules.assignment_report(),
            (0, 0),
            "the recount follows the operator's fix, so the alarm clears"
        );
    }

    #[tokio::test]
    async fn an_account_registered_with_a_mixed_case_id_takes_layers_and_assignments() {
        let mut state = state();
        state.metadata = Arc::new(MockMetadataStore::new().with_row(AccountMetadata {
            account_id: "0xAcC".into(),
            auth: Auth::MidenFalconRpo {
                cosigner_commitments: vec!["0xs1".into()],
            },
            network_config: crate::metadata::NetworkConfig::miden_default(),
            created_at: "2026-10-15T00:00:00Z".into(),
            updated_at: "2026-10-15T00:00:00Z".into(),
            has_pending_candidate: false,
            paused_at: None,
            paused_reason: None,
            released_at: None,
        }));
        set_layer(
            &state,
            &operator(),
            "0xAcC",
            "blocklist",
            Some(json!({ "denylist": ["0x000000000000000000000000000bbb"] })),
            "OP-1",
            None,
        )
        .await
        .expect("the id as registered is found");
        let rows = state
            .modules
            .as_ref()
            .unwrap()
            .store()
            .account_params("0xacc")
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "stored under the key evaluation reads");
        assert!(matches!(
            set_layer(
                &state,
                &operator(),
                "0xBbB",
                "blocklist",
                None,
                "OP-2",
                None
            )
            .await,
            Err(GuardianError::AccountNotFound(_))
        ));
    }

    #[tokio::test]
    async fn a_layer_left_by_a_scope_change_can_be_deleted_before_the_module_is_assigned() {
        let state = state();
        let store = state.modules.as_ref().unwrap().store_handle();
        store
            .set_layer(
                ACCOUNT,
                "optional",
                Some(json!({ "denylist": ["0x000000000000000000000000000bbb"] })),
                &Change {
                    operator: "0xop",
                    reason: "written while optional applied to every account",
                    at: "t0".into(),
                    expected_version: None,
                },
            )
            .await
            .unwrap();
        let operator = operator();
        let assign = || {
            set_assignment(
                &state,
                &operator,
                ACCOUNT,
                vec![Assigned {
                    module_id: "optional".into(),
                    params: json!({}),
                }],
                "OP-1",
                None,
            )
        };
        assert!(
            matches!(assign().await, Err(GuardianError::ModuleWriteRefused(message)) if message.contains("delete the layer first"))
        );
        set_layer(&state, &operator, ACCOUNT, "optional", None, "OP-2", None)
            .await
            .expect("a stale layer can be deleted whatever the instance's scope is now");
        assign().await.expect("then the module can be assigned");
    }

    #[tokio::test]
    async fn a_layer_is_refused_while_the_module_is_still_assigned_to_the_account() {
        let mut state = state();
        let operator = operator();
        set_assignment(
            &state,
            &operator,
            ACCOUNT,
            vec![Assigned {
                module_id: "optional".into(),
                params: json!({}),
            }],
            "OP-1",
            None,
        )
        .await
        .unwrap();
        let hooks = [HookName::BeforeTransactionApproved];
        let store = state.modules.as_ref().unwrap().store_handle();
        state.modules = Some(Arc::new(ModulePipeline::from_instances_with_store(
            vec![instance(
                "optional",
                &hooks,
                OnError::Deny,
                Arc::new(RecipientPolicy),
            )],
            store,
        )));
        let layer = set_layer(
            &state,
            &operator,
            ACCOUNT,
            "optional",
            Some(json!({})),
            "OP-2",
            None,
        )
        .await;
        assert!(
            matches!(layer, Err(GuardianError::ModuleWriteRefused(message)) if message.contains("remove it from the account's assignment first")),
            "the scope change left an assignment row for the module"
        );
    }

    #[tokio::test]
    async fn a_reason_longer_than_the_limit_is_refused() {
        let state = state();
        let refused = set_layer(
            &state,
            &operator(),
            ACCOUNT,
            "blocklist",
            Some(json!({})),
            &"r".repeat(MAX_REASON_CHARS + 1),
            None,
        )
        .await;
        assert!(
            matches!(refused, Err(GuardianError::ModuleWriteRefused(message)) if message.contains("character limit"))
        );
    }

    #[tokio::test]
    async fn an_account_already_over_the_budget_can_be_brought_back_under_it() {
        let mut state = state();
        let optional = || {
            vec![Assigned {
                module_id: "optional".into(),
                params: json!({}),
            }]
        };
        set_assignment(&state, &operator(), ACCOUNT, optional(), "OP-1", None)
            .await
            .unwrap();
        let store = state.modules.as_ref().unwrap().store_handle();
        let instances = state.modules.as_ref().unwrap().instances().to_vec();
        state.modules = Some(Arc::new(
            ModulePipeline::from_instances_with_store(instances, store).with_limits(
                crate::modules::limits::Limits {
                    budget: std::time::Duration::from_millis(120),
                    ..Default::default()
                },
            ),
        ));

        let more = set_layer(
            &state,
            &operator(),
            ACCOUNT,
            "blocklist",
            Some(json!({ "denylist": ["0x000000000000000000000000000bbb"] })),
            "OP-2",
            None,
        )
        .await;
        assert!(
            matches!(&more, Err(GuardianError::ModuleWriteRefused(message)) if message.contains("budget")),
            "a write that adds cost to an account over the budget: {more:?}"
        );
        set_assignment(&state, &operator(), ACCOUNT, optional(), "OP-3", None)
            .await
            .expect("the same cost is not refused");
        set_assignment(&state, &operator(), ACCOUNT, Vec::new(), "OP-4", None)
            .await
            .expect("removing the assignment brings the account back under the budget");
    }

    #[tokio::test]
    async fn global_and_account_writes_are_validated_recorded_and_visible() {
        let state = state();
        let operator = operator();
        set_global(
            &state,
            &operator,
            "blocklist",
            json!({ "denylist": ["0x000000000000000000000000000aaa", "0x000000000000000000000000000ddd"] }),
            "OP-90",
            None,
        )
        .await
        .unwrap();
        let view = set_layer(
            &state,
            &operator,
            ACCOUNT,
            "blocklist",
            Some(json!({ "denylist": ["0x000000000000000000000000000ccc"] })),
            "OP-91",
            None,
        )
        .await
        .unwrap();
        let entry = &view.modules[0];
        assert_eq!(
            entry.global_params,
            Some(
                json!({ "denylist": ["0x000000000000000000000000000aaa", "0x000000000000000000000000000ddd"] })
            )
        );
        assert_eq!(
            entry.account_layer,
            Some(json!({ "denylist": ["0x000000000000000000000000000ccc"] }))
        );

        let history = history(&state, "blocklist", None, None)
            .await
            .unwrap()
            .entries;
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].account_id.as_deref(),
            Some(ACCOUNT),
            "newest first"
        );
        assert_eq!(history[1].reason, "OP-90");
    }

    /// An account write is validated and budget-checked against the rows it read, so it is
    /// always fenced on their version: the operator's when given, the read's otherwise.
    #[tokio::test]
    async fn an_account_write_is_fenced_on_the_version_it_read() {
        let state = state();
        let operator = operator();
        let unversioned = account_change(&state, &operator, Write::from("no version"), 7).unwrap();
        assert_eq!(unversioned.expected_version, Some(7));
        let current = Write {
            reason: "operator's version",
            expected_version: Some(7),
        };
        assert_eq!(
            account_change(&state, &operator, current, 7)
                .unwrap()
                .expected_version,
            Some(7)
        );
        for other in [5, 8] {
            let write = Write {
                reason: "a version the rows were not read at",
                expected_version: Some(other),
            };
            assert!(matches!(
                account_change(&state, &operator, write, 7),
                Err(GuardianError::ModuleParamsStale)
            ));
        }
    }

    #[tokio::test]
    async fn a_write_made_against_a_version_that_moved_is_refused() {
        let state = state();
        let operator = operator();
        set_global(&state, &operator, "blocklist", json!({}), "OP-1", None)
            .await
            .unwrap();
        let read = state
            .modules
            .as_ref()
            .unwrap()
            .store()
            .instance_version()
            .await
            .unwrap();
        let against = |reason| Write {
            reason,
            expected_version: Some(read),
        };
        set_global(
            &state,
            &operator,
            "blocklist",
            json!({ "denylist": ["0x000000000000000000000000000aaa"] }),
            against("first editor"),
            None,
        )
        .await
        .expect("the version it read is current");
        let second = set_global(
            &state,
            &operator,
            "blocklist",
            json!({ "denylist": ["0x000000000000000000000000000bbb"] }),
            against("second editor, same read"),
            None,
        )
        .await;
        assert!(
            matches!(second, Err(GuardianError::ModuleParamsStale)),
            "{second:?}"
        );

        let account_read = set_layer(
            &state,
            &operator,
            ACCOUNT,
            "blocklist",
            Some(json!({})),
            "OP-2",
            None,
        )
        .await
        .unwrap()
        .account_params_version;
        let stale_removal = set_layer(
            &state,
            &operator,
            ACCOUNT,
            "blocklist",
            None,
            Write {
                reason: "removal from an older read",
                expected_version: Some(account_read - 1),
            },
            None,
        )
        .await;
        assert!(matches!(
            stale_removal,
            Err(GuardianError::ModuleParamsStale)
        ));
        let removal = set_layer(
            &state,
            &operator,
            ACCOUNT,
            "blocklist",
            None,
            Write {
                reason: "removal from the current read",
                expected_version: Some(account_read),
            },
            None,
        )
        .await;
        assert!(removal.is_ok(), "{removal:?}");

        let history = history(&state, "blocklist", None, None)
            .await
            .unwrap()
            .entries;
        assert!(
            history
                .iter()
                .all(|entry| !entry.reason.starts_with("second editor")
                    && entry.reason != "removal from an older read"),
            "a refused write leaves no history"
        );
    }

    #[tokio::test]
    async fn history_pages_newest_first_by_version() {
        let state = state();
        for n in 0..3 {
            set_global(
                &state,
                &operator(),
                "blocklist",
                json!({ "denylist": [format!("0x{n:030x}")] }),
                &format!("OP-{n}"),
                None,
            )
            .await
            .unwrap();
        }
        let first = history(&state, "blocklist", None, Some(2))
            .await
            .unwrap()
            .entries;
        assert_eq!(
            first
                .iter()
                .map(|entry| entry.reason.as_str())
                .collect::<Vec<_>>(),
            ["OP-2", "OP-1"]
        );
        let next = history(&state, "blocklist", Some(first[1].version), Some(2))
            .await
            .unwrap()
            .entries;
        assert_eq!(
            next.iter()
                .map(|entry| entry.reason.as_str())
                .collect::<Vec<_>>(),
            ["OP-0"]
        );
        assert!(history(&state, "blocklist", None, Some(0)).await.is_err());
    }

    #[tokio::test]
    async fn params_over_the_size_cap_are_refused_before_the_module_is_called() {
        let state = state();
        let huge = json!({ "denylist": vec!["0x".repeat(40); 4_000] });
        let refused = set_global(&state, &operator(), "blocklist", huge, "OP-9", None).await;
        assert!(
            matches!(&refused, Err(GuardianError::ModuleWriteRefused(message)) if message.contains("GUARDIAN_MODULE_PARAMS_MAX_BYTES")),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn invalid_writes_are_refused() {
        let state = state();
        let operator = operator();
        let cases = [
            set_global(
                &state,
                &operator,
                "blocklist",
                json!({ "unknown": 1 }),
                "r",
                None,
            )
            .await
            .err(),
            set_global(
                &state,
                &operator,
                "blocklist",
                json!({ "k": { "$secret": "x" } }),
                "r",
                None,
            )
            .await
            .err(),
            set_global(&state, &operator, "blocklist", json!({}), " ", None)
                .await
                .err(),
            set_global(&state, &operator, "vendor", json!({}), "r", None)
                .await
                .err(),
            set_layer(
                &state,
                &operator,
                ACCOUNT,
                "optional",
                Some(json!({})),
                "r",
                None,
            )
            .await
            .err(),
            set_layer(
                &state,
                &operator,
                "0xmissing",
                "blocklist",
                Some(json!({})),
                "r",
                None,
            )
            .await
            .err(),
            set_assignment(
                &state,
                &operator,
                ACCOUNT,
                vec![Assigned {
                    module_id: "blocklist".into(),
                    params: json!({}),
                }],
                "r",
                None,
            )
            .await
            .err(),
        ];
        for (index, case) in cases.into_iter().enumerate() {
            assert!(case.is_some(), "case {index} should be refused");
        }
        assert!(matches!(
            set_global(&state, &operator, "nope", json!({}), "r", None).await,
            Err(GuardianError::ModuleNotFound { .. })
        ));
    }

    #[tokio::test]
    async fn disable_and_enable_follow_the_catalog_rules() {
        let state = state();
        let operator = operator();
        let pipeline = state.modules.as_deref().unwrap();
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

        let view = disable(&state, &operator, "blocklist", "faulty vendor", None)
            .await
            .unwrap();
        assert!(!view.enabled);
        assert!(view.enabled_drift);
        let view = enable(&state, &operator, "blocklist", "vendor fixed", None)
            .await
            .unwrap();
        assert!(view.enabled);
        let history = history(&state, "blocklist", None, None)
            .await
            .unwrap()
            .entries;
        assert_eq!(history[0].new, json!({ "enabled": true }), "newest first");
    }

    #[tokio::test]
    async fn an_instance_the_catalog_protects_cannot_be_disabled() {
        let state = state();
        let pipeline = state.modules.as_deref().unwrap();
        assert!(pipeline.instance("vendor").is_some());
        let result = disable(&state, &operator(), "protected", "r", None).await;
        assert!(matches!(result, Err(GuardianError::ModuleNotFound { .. })));
        let result = disable(&state, &operator(), "vendor", "r", None).await;
        assert!(matches!(
            result,
            Err(GuardianError::ModuleNotDisableable { .. })
        ));
    }

    #[tokio::test]
    async fn optional_modules_are_assigned_per_account() {
        let state = state();
        let view = set_assignment(
            &state,
            &operator(),
            ACCOUNT,
            vec![Assigned {
                module_id: "optional".into(),
                params: json!({ "denylist": ["0x000000000000000000000000000eee"] }),
            }],
            "customer request",
            None,
        )
        .await
        .unwrap();
        assert!(
            view.modules
                .iter()
                .any(|m| m.module_id == "optional" && m.params.is_some())
        );
    }
}
