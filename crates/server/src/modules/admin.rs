//! Operator management of module parameters: global params, account layers, assignments.

use serde::Serialize;

use super::catalog::{ClientReason, Disclosure, Scope, contains_secret_reference};
use super::pipeline::ModulePipeline;
use super::registry::{Instance, validate_params};
use super::store::{Assigned, Change, HistoryEntry, ParamsKind};
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
        .ok_or_else(|| GuardianError::InvalidInput("no policy modules are configured".into()))
}

fn instance<'a>(pipeline: &'a ModulePipeline, module_id: &str) -> Result<&'a Instance> {
    pipeline
        .instance(module_id)
        .ok_or_else(|| GuardianError::ModuleNotFound {
            module: module_id.to_string(),
        })
}

fn require_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        return Err(GuardianError::InvalidInput("reason is required".into()));
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
        return Err(GuardianError::InvalidInput(
            "params must be a JSON object".into(),
        ));
    }
    pipeline
        .limits()
        .check_params_size(params)
        .map_err(|e| GuardianError::InvalidInput(format!("module {}: {e}", instance.config.id)))?;
    if contains_secret_reference(params) {
        return Err(GuardianError::InvalidInput(
            "secret references cannot be set through the operator API".into(),
        ));
    }
    let stored = validate_params(&instance.module, params, instance.config.timeout)
        .await
        .map_err(|e| GuardianError::InvalidInput(format!("module {}: {e}", instance.config.id)))?;
    pipeline
        .storable_normalization(&stored)
        .map_err(|e| GuardianError::InvalidInput(format!("module {}: {e}", instance.config.id)))?;
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

async fn version(pipeline: &ModulePipeline) -> Result<u64> {
    pipeline
        .store()
        .version()
        .await
        .map_err(GuardianError::StorageError)
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
    }
}

async fn instance_view(
    pipeline: &ModulePipeline,
    instance: &Instance,
) -> Result<ModuleInstanceView> {
    let params = pipeline.global_params(instance).await?;
    let enabled = pipeline.enabled(instance).await?;
    let seeded = pipeline
        .store()
        .instances()
        .await
        .map_err(GuardianError::StorageError)?
        .into_iter()
        .find(|row| row.module_id == instance.config.id)
        .map(|row| row.seeded_params);
    Ok(ModuleInstanceView {
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
        params_drift: seeded.is_some_and(|seeded| seeded != instance.config.params),
        params,
        file_managed_params: instance.config.file_managed_params,
        build: instance.build.clone(),
        disclosure: instance.config.disclosure,
        description: instance.config.description.clone(),
        client_reason: instance.config.client_reason,
        dry_run: instance.config.dry_run,
        circuit: pipeline.breakers().state(&instance.config.id).as_str(),
    })
}

pub async fn catalog_view(state: &AppState) -> Result<ModuleCatalogView> {
    let pipeline = pipeline(state)?;
    let mut modules = Vec::with_capacity(pipeline.instances().len());
    for instance in pipeline.instances() {
        modules.push(instance_view(pipeline, instance).await?);
    }
    let undeclared = pipeline
        .store()
        .instances()
        .await
        .map_err(GuardianError::StorageError)?
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
        params_version: version(pipeline).await?,
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
    let rows = pipeline
        .store()
        .account_params(account_id)
        .await
        .map_err(GuardianError::StorageError)?;
    let mut modules = Vec::new();
    for instance in pipeline
        .instances()
        .iter()
        .filter(|instance| instance.config.scope == Scope::All)
    {
        modules.push(AccountModuleEntry {
            module_id: instance.config.id.clone(),
            scope: "all",
            enabled: pipeline.enabled(instance).await?,
            orphaned: false,
            global_params: Some(pipeline.global_params(instance).await?),
            account_layer: rows
                .iter()
                .find(|row| row.kind == ParamsKind::Layer && row.module_id == instance.config.id)
                .map(|row| row.params.clone()),
            params: None,
        });
    }
    for row in rows
        .iter()
        .filter(|row| row.kind == ParamsKind::Layer && pipeline.instance(&row.module_id).is_none())
    {
        modules.push(AccountModuleEntry {
            module_id: row.module_id.clone(),
            scope: "all",
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
                Some(instance) => pipeline.enabled(instance).await?,
                None => false,
            },
            orphaned: instance.is_none(),
            module_id: row.module_id,
            global_params: None,
            account_layer: None,
            params: Some(row.params),
        });
    }
    Ok(AccountModulesView {
        account_id: account_id.to_string(),
        modules,
    })
}

const HISTORY_PAGE_DEFAULT: usize = 100;
const HISTORY_PAGE_MAX: usize = 1_000;

/// The instance's params changes, newest first: those with a version below `before` when set,
/// at most `limit` (default 100, at most 1,000).
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
        return Err(GuardianError::InvalidInput(format!(
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
                    },
                )
                .await
                .map_err(GuardianError::StorageError)?;
            if !migrated {
                tracing::warn!(module = %module_id, "Global params changed while the module migrated them; kept the newer value");
            }
        }
        pipeline
            .store()
            .set_enabled(module_id, true, &change(state, operator, reason))
            .await
            .map_err(GuardianError::StorageError)?;
        super::metrics::enabled(module_id, true);
    }
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "params_version": version(pipeline).await? });
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
    if pipeline.enabled(instance).await? {
        pipeline
            .store()
            .set_enabled(module_id, false, &change(state, operator, reason))
            .await
            .map_err(GuardianError::StorageError)?;
        super::metrics::enabled(module_id, false);
    }
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "params_version": version(pipeline).await? });
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

pub async fn set_global(
    state: &AppState,
    operator: &AuthenticatedOperator,
    module_id: &str,
    params: serde_json::Value,
    reason: &str,
    client_ip: Option<String>,
) -> Result<ModuleInstanceView> {
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let instance = instance(pipeline, module_id)?;
    if instance.config.file_managed_params {
        return Err(GuardianError::InvalidInput(format!(
            "module {module_id}: params with secret references are managed in the catalog file"
        )));
    }
    let params = validated(pipeline, instance, &params).await?;
    pipeline
        .store()
        .set_global(module_id, params, &change(state, operator, reason))
        .await
        .map_err(GuardianError::StorageError)?;
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "params_version": version(pipeline).await? });
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

pub async fn set_layer(
    state: &AppState,
    operator: &AuthenticatedOperator,
    account_id: &str,
    module_id: &str,
    params: Option<serde_json::Value>,
    reason: &str,
    client_ip: Option<String>,
) -> Result<AccountModulesView> {
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let account_id = existing_account(state, account_id).await?;
    let instance = instance(pipeline, module_id)?;
    if instance.config.scope != Scope::All {
        return Err(GuardianError::InvalidInput(format!(
            "module {module_id}: account layers apply only to scope = \"all\" instances; assign it instead"
        )));
    }
    let params = match params {
        Some(params) => Some(validated(pipeline, instance, &params).await?),
        None => None,
    };
    let current = pipeline
        .store()
        .account_params(&account_id)
        .await
        .map_err(GuardianError::StorageError)?;
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
    pipeline
        .store()
        .set_layer(
            &account_id,
            module_id,
            params,
            &change(state, operator, reason),
        )
        .await
        .map_err(GuardianError::StorageError)?;
    let payload = serde_json::json!({ "module_id": module_id, "reason": reason, "params_version": version(pipeline).await? });
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

pub async fn set_assignment(
    state: &AppState,
    operator: &AuthenticatedOperator,
    account_id: &str,
    modules: Vec<Assigned>,
    reason: &str,
    client_ip: Option<String>,
) -> Result<AccountModulesView> {
    require_reason(reason)?;
    let pipeline = pipeline(state)?;
    let account_id = existing_account(state, account_id).await?;
    let mut to_store = Vec::with_capacity(modules.len());
    for (index, assigned) in modules.iter().enumerate() {
        let instance = instance(pipeline, &assigned.module_id)?;
        if instance.config.scope == Scope::All {
            return Err(GuardianError::InvalidInput(format!(
                "module {}: scope = \"all\" applies to every account; set an account layer instead",
                assigned.module_id
            )));
        }
        if modules[..index]
            .iter()
            .any(|earlier| earlier.module_id == assigned.module_id)
        {
            return Err(GuardianError::InvalidInput(format!(
                "module {} listed twice",
                assigned.module_id
            )));
        }
        to_store.push(Assigned {
            module_id: assigned.module_id.clone(),
            params: validated(pipeline, instance, &assigned.params).await?,
        });
    }
    let current = pipeline
        .store()
        .account_params(&account_id)
        .await
        .map_err(GuardianError::StorageError)?;
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
    pipeline
        .store()
        .set_assignment(&account_id, to_store, &change(state, operator, reason))
        .await
        .map_err(GuardianError::StorageError)?;
    let payload =
        serde_json::json!({ "reason": reason, "params_version": version(pipeline).await? });
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

    use guardian_module_sdk::basic_policy::BasicPolicy;
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
        let mut optional = instance("optional", &hooks, OnError::Deny, Arc::new(BasicPolicy));
        optional.config.scope = Scope::Assigned;
        let mut secret = instance("vendor", &hooks, OnError::Deny, Arc::new(BasicPolicy));
        secret.config.file_managed_params = true;
        secret.config.operator_can_disable = false;
        state.modules = Some(pipeline(vec![
            instance_with_params(
                "blocklist",
                &hooks,
                Arc::new(BasicPolicy),
                json!({ "recipient_denylist": ["0xaaa"] }),
            ),
            optional,
            secret,
        ]));
        state
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
            Arc::new(BasicPolicy),
        );
        screening.config.scope = Scope::Default;
        instances.push(screening);
        store
            .seed_instance("retired", true, &json!({ "old": true }), "t0")
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
    }

    #[tokio::test]
    async fn the_catalog_view_shows_each_instance_circuit_on_this_replica() {
        let state = state();
        let pipeline = state.modules.as_ref().unwrap();
        for _ in 0..crate::modules::limits::Limits::default().breaker_failures {
            pipeline.breakers().failed("blocklist");
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
            Some(json!({ "recipient_denylist": ["0xbbb"] })),
            "OP-1",
            None,
        )
        .await;
        assert!(
            matches!(&layer, Err(GuardianError::InvalidInput(message)) if message.contains("budget")),
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
        assert!(matches!(assignment, Err(GuardianError::InvalidInput(_))));
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
            Some(json!({ "recipient_denylist": ["0xbbb"] })),
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
            Some(json!({ "recipient_denylist": ["0xbbb"] })),
            "OP-2",
            None,
        )
        .await;
        assert!(
            matches!(&more, Err(GuardianError::InvalidInput(message)) if message.contains("budget")),
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
            json!({ "recipient_denylist": ["0xaaa", "0xddd"] }),
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
            Some(json!({ "recipient_denylist": ["0xccc"] })),
            "OP-91",
            None,
        )
        .await
        .unwrap();
        let entry = &view.modules[0];
        assert_eq!(
            entry.global_params,
            Some(json!({ "recipient_denylist": ["0xaaa", "0xddd"] }))
        );
        assert_eq!(
            entry.account_layer,
            Some(json!({ "recipient_denylist": ["0xccc"] }))
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

    #[tokio::test]
    async fn history_pages_newest_first_by_version() {
        let state = state();
        for n in 0..3 {
            set_global(
                &state,
                &operator(),
                "blocklist",
                json!({ "recipient_denylist": [format!("0x{n}")] }),
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
        let huge = json!({ "recipient_denylist": vec!["0x".repeat(40); 4_000] });
        let refused = set_global(&state, &operator(), "blocklist", huge, "OP-9", None).await;
        assert!(
            matches!(&refused, Err(GuardianError::InvalidInput(message)) if message.contains("GUARDIAN_MODULE_PARAMS_MAX_BYTES")),
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
                &json!({ "recipient_denylist": ["0xaaa"] }),
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
                params: json!({ "recipient_denylist": ["0xeee"] }),
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
