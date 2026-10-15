//! Operator management of module parameters: global params, account layers, assignments.

use serde::Serialize;

use super::catalog::{Scope, contains_secret_reference};
use super::pipeline::ModulePipeline;
use super::registry::{Instance, validate_params};
use super::store::{Assigned, Change, HistoryEntry, ParamsKind};
use crate::audit::{AuditEvent, AuditOutcome, kinds};
use crate::dashboard::AuthenticatedOperator;
use crate::error::{GuardianError, Result};
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
pub struct ModuleInstanceView {
    pub module_id: String,
    pub kind: String,
    pub scope: &'static str,
    pub hooks: Vec<String>,
    pub enabled: bool,
    pub catalog_enabled: bool,
    pub enabled_drift: bool,
    pub operator_can_disable: bool,
    pub params: serde_json::Value,
    pub params_drift: bool,
    pub file_managed_params: bool,
    /// `name version` from `describe` at startup; absent when the startup checks failed.
    pub build: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModuleCatalogView {
    pub catalog_hash: String,
    pub params_version: u64,
    pub modules: Vec<ModuleInstanceView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountModuleEntry {
    pub module_id: String,
    pub scope: &'static str,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_params: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_layer: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AccountModulesView {
    pub account_id: String,
    pub modules: Vec<AccountModuleEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ParamsHistoryView {
    pub module_id: String,
    pub entries: Vec<HistoryEntry>,
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::All => "all",
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

async fn validated(instance: &Instance, params: &serde_json::Value) -> Result<()> {
    if !params.is_object() {
        return Err(GuardianError::InvalidInput(
            "params must be a JSON object".into(),
        ));
    }
    if contains_secret_reference(params) {
        return Err(GuardianError::InvalidInput(
            "secret references cannot be set through the operator API".into(),
        ));
    }
    validate_params(instance.module.as_ref(), params, instance.config.timeout)
        .await
        .map_err(|e| GuardianError::InvalidInput(format!("module {}: {e}", instance.config.id)))
}

async fn existing_account(state: &AppState, account_id: &str) -> Result<String> {
    let account_id = account_id.to_ascii_lowercase();
    match state.metadata.get(&account_id).await {
        Ok(Some(_)) => Ok(account_id),
        Ok(None) => Err(GuardianError::AccountNotFound(account_id)),
        Err(e) => Err(GuardianError::StorageError(format!(
            "account lookup failed: {e}"
        ))),
    }
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
    Ok(ModuleInstanceView {
        module_id: instance.config.id.clone(),
        kind: instance.config.kind.clone(),
        scope: scope_name(instance.config.scope),
        hooks: instance
            .config
            .hooks
            .iter()
            .map(|hook| {
                let name = hook.proto().as_str_name().to_ascii_lowercase();
                name.strip_prefix("hook_").unwrap_or(&name).to_string()
            })
            .collect(),
        enabled,
        catalog_enabled: instance.config.catalog_enabled,
        enabled_drift: enabled != instance.config.catalog_enabled,
        operator_can_disable: instance.config.operator_can_disable,
        params_drift: params != instance.config.params,
        params,
        file_managed_params: instance.config.file_managed_params,
        build: instance.build.clone(),
    })
}

pub async fn catalog_view(state: &AppState) -> Result<ModuleCatalogView> {
    let pipeline = pipeline(state)?;
    let mut modules = Vec::with_capacity(pipeline.instances().len());
    for instance in pipeline.instances() {
        modules.push(instance_view(pipeline, instance).await?);
    }
    Ok(ModuleCatalogView {
        catalog_hash: pipeline.catalog_hash.clone(),
        params_version: version(pipeline).await?,
        modules,
    })
}

pub async fn account_view(state: &AppState, account_id: &str) -> Result<AccountModulesView> {
    let pipeline = pipeline(state)?;
    let account_id = existing_account(state, account_id).await?;
    let rows = pipeline
        .store()
        .account_params(&account_id)
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
            global_params: Some(pipeline.global_params(instance).await?),
            account_layer: rows
                .iter()
                .find(|row| row.kind == ParamsKind::Layer && row.module_id == instance.config.id)
                .map(|row| row.params.clone()),
            params: None,
        });
    }
    modules.extend(
        rows.into_iter()
            .filter(|row| row.kind == ParamsKind::Assignment)
            .map(|row| AccountModuleEntry {
                scope: "assigned",
                module_id: row.module_id,
                global_params: None,
                account_layer: None,
                params: Some(row.params),
                enabled: false,
            }),
    );
    for entry in modules.iter_mut().filter(|entry| entry.scope == "assigned") {
        entry.enabled = match pipeline.instance(&entry.module_id) {
            Some(instance) => pipeline.enabled(instance).await?,
            None => false,
        };
    }
    Ok(AccountModulesView {
        account_id,
        modules,
    })
}

pub async fn history(state: &AppState, module_id: &str) -> Result<ParamsHistoryView> {
    let pipeline = pipeline(state)?;
    instance(pipeline, module_id)?;
    Ok(ParamsHistoryView {
        module_id: module_id.to_string(),
        entries: pipeline
            .store()
            .history(module_id)
            .await
            .map_err(GuardianError::StorageError)?,
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
        super::registry::check(&instance.config, instance.module.as_ref(), &params)
            .await
            .map(|_| ())
            .map_err(|error| {
                tracing::warn!(module = %module_id, %error, "Module failed its checks; not enabled");
                GuardianError::ModuleUnavailable {
                    module: module_id.to_string(),
                }
            })?;
        pipeline
            .store()
            .set_enabled(module_id, true, &change(state, operator, reason))
            .await
            .map_err(GuardianError::StorageError)?;
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
    validated(instance, &params).await?;
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
    if let Some(params) = &params {
        validated(instance, params).await?;
    }
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
    account_view(state, &account_id).await
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
        validated(instance, &assigned.params).await?;
    }
    pipeline
        .store()
        .set_assignment(&account_id, modules, &change(state, operator, reason))
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
    account_view(state, &account_id).await
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

        let history = history(&state, "blocklist").await.unwrap().entries;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].reason, "OP-90");
        assert_eq!(history[1].account_id.as_deref(), Some(ACCOUNT));
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
        let history = history(&state, "blocklist").await.unwrap().entries;
        assert_eq!(history.last().unwrap().new, json!({ "enabled": true }));
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
