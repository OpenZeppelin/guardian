//! What an account's cosigners may learn about the policies that apply to them (RFC 0002
//! section 2.7).

use serde::{Deserialize, Serialize};

use super::catalog::{Disclosure, Scope};
use super::pipeline::ModulePipeline;
use super::store::ParamsKind;
use crate::error::GuardianError;

/// One policy as the account's cosigners see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PolicyEntry {
    pub module_id: String,
    pub hooks: Vec<String>,
    pub description: String,
    /// `disclosure = "params"`: the instance's global params, or the account's own params for
    /// an assigned instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub params: Option<serde_json::Value>,
    /// `disclosure = "params"` on a `scope = all` instance: this account's layer, when set.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub account_layer: Option<serde_json::Value>,
}

impl ModulePipeline {
    /// The enabled instances that apply to the account, in evaluation order, without those
    /// whose catalog entry discloses nothing.
    pub async fn disclosed(&self, account_id: &str) -> Result<Vec<PolicyEntry>, GuardianError> {
        let account_id = account_id.to_ascii_lowercase();
        let (_, rows, account_rows) = self.snapshot(&account_id).await?;
        let entry = |instance: &super::registry::Instance,
                     params: Option<serde_json::Value>,
                     account_layer: Option<serde_json::Value>| {
            let config = &instance.config;
            let show_params = config.disclosure == Disclosure::Params;
            PolicyEntry {
                module_id: config.id.clone(),
                hooks: config
                    .hooks
                    .iter()
                    .map(|hook| hook.as_str().to_string())
                    .collect(),
                description: config.description.clone().unwrap_or_default(),
                params: params.filter(|_| show_params),
                account_layer: account_layer.filter(|_| show_params),
            }
        };
        let visible = |instance: &super::registry::Instance| {
            instance.config.disclosure != Disclosure::None && Self::enabled_in(&rows, instance)
        };
        let mut policies = Vec::new();
        for instance in self
            .instances()
            .iter()
            .filter(|instance| instance.config.scope == Scope::All && visible(instance))
        {
            let layer = account_rows
                .iter()
                .find(|row| row.kind == ParamsKind::Layer && row.module_id == instance.config.id)
                .map(|row| row.params.clone());
            policies.push(entry(
                instance,
                Some(Self::global_params_in(&rows, instance)),
                layer,
            ));
        }
        for row in account_rows
            .iter()
            .filter(|row| row.kind == ParamsKind::Assignment)
        {
            if let Some(instance) = self
                .instance(&row.module_id)
                .filter(|instance| instance.config.scope != Scope::All && visible(instance))
            {
                policies.push(entry(instance, Some(row.params.clone()), None));
            }
        }
        Ok(policies)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::modules::catalog::HookName;
    use crate::modules::store::{Assigned, Change};
    use crate::modules::testing::{StubModule, instance_with_params, pipeline};

    const ACCOUNT: &str = "0xacc";

    fn change() -> Change<'static> {
        Change {
            operator: "0xop",
            reason: "test",
            at: "2026-10-16T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn only_disclosed_enabled_policies_are_listed_with_what_they_allow() {
        let hooks = [HookName::BeforeTransactionApproved];
        let module = || StubModule::allowing() as std::sync::Arc<dyn guardian_module_sdk::Module>;
        let with = |id: &str, disclosure: Disclosure, scope: Scope| {
            let mut instance = instance_with_params(id, &hooks, module(), json!({ "cap": id }));
            instance.config.disclosure = disclosure;
            instance.config.scope = scope;
            if disclosure != Disclosure::None {
                instance.config.description = Some(format!("{id} rule"));
            }
            instance
        };
        let pipeline = pipeline(vec![
            with("caps", Disclosure::Params, Scope::All),
            with("screening", Disclosure::None, Scope::All),
            with("summary", Disclosure::Summary, Scope::All),
            with("paused", Disclosure::Summary, Scope::All),
            with("treasury", Disclosure::Params, Scope::Assigned),
        ]);
        let store = pipeline.store();
        for id in ["caps", "screening", "summary", "paused", "treasury"] {
            store
                .seed_instance(id, true, &json!({ "cap": id }), "2026-10-16T00:00:00Z")
                .await
                .unwrap();
        }
        store.set_enabled("paused", false, &change()).await.unwrap();
        store
            .set_layer(
                ACCOUNT,
                "caps",
                Some(json!({ "cap": "tighter" })),
                &change(),
            )
            .await
            .unwrap();
        store
            .set_layer(
                ACCOUNT,
                "summary",
                Some(json!({ "cap": "secret" })),
                &change(),
            )
            .await
            .unwrap();
        store
            .set_assignment(
                ACCOUNT,
                vec![Assigned {
                    module_id: "treasury".into(),
                    params: json!({ "cap": "own" }),
                }],
                &change(),
            )
            .await
            .unwrap();

        let policies = pipeline.disclosed(ACCOUNT).await.unwrap();
        let ids: Vec<_> = policies.iter().map(|p| p.module_id.as_str()).collect();
        assert_eq!(
            ids,
            ["caps", "summary", "treasury"],
            "none and disabled are absent"
        );
        assert_eq!(policies[0].params, Some(json!({ "cap": "caps" })));
        assert_eq!(policies[0].account_layer, Some(json!({ "cap": "tighter" })));
        assert_eq!(policies[1].description, "summary rule");
        assert_eq!(policies[1].params, None, "summary shows no params");
        assert_eq!(policies[1].account_layer, None, "nor the account layer");
        assert_eq!(policies[2].params, Some(json!({ "cap": "own" })));
        assert_eq!(policies[2].hooks, ["before_transaction_approved"]);
    }
}
