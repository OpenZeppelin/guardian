use std::collections::BTreeSet;

use super::{Action, Availability, Manifest, Profile, Runtime, Scheme, Sdk, SdkSelector};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("scenario at position {position} has an empty id")]
    EmptyId { position: usize },

    #[error("scenario id `{id}` is declared more than once")]
    DuplicateId { id: String },

    #[error(
        "scenario `{id}` declares offline mode but its actions are not guardian migration; \
         every other proposal type must be created online before it can be exported"
    )]
    OfflineCreationOutsideMigration { id: String },

    #[error("scenario `{id}` declares the browser runtime but targets the Rust SDK")]
    BrowserRuntimeOnRustSdk { id: String },

    #[error(
        "scenario `{id}` has a step budget of {budget_seconds}s but is required on {network}, \
         whose historical window is {window_seconds}s; mark it not required for that network \
         rather than shrinking a budget the flow cannot meet"
    )]
    BudgetExceedsWindow {
        id: String,
        network: String,
        budget_seconds: u64,
        window_seconds: u64,
    },

    #[error("scenario `{id}` is required on {network}/{sdk}, which is declared unavailable")]
    RequiredOnUnavailablePair {
        id: String,
        network: String,
        sdk: String,
    },

    #[error(
        "scenario `{id}` names a mixed signature scheme outside the deterministic Rust leg; \
         neither multisig SDK can construct a mixed-scheme account yet (#549, and the \
         TypeScript follow-up to #540), so only the driver's own builder can, against the \
         server alone"
    )]
    MixedScheme { id: String },

    #[error("scenario `{id}` names a scheme outside falcon, ecdsa, mixed and n/a")]
    UnrecognizedScheme { id: String },

    #[error(
        "scenario `{id}` must be labelled `scheme = \"mixed\"` exactly when its actions are \
         `mixed-scheme-*` ones; a mixed label on other actions would claim coverage no \
         mixed-scheme account produced"
    )]
    MixedSchemeActions { id: String },

    #[error("scenario `{id}` names action `{action}`, which is outside the vocabulary")]
    UnknownAction { id: String, action: String },

    #[error("scenario `{id}` declares a TypeScript target without a runtime")]
    MissingRuntime { id: String },

    #[error("matrix references network `{network}`, which is not declared")]
    UndeclaredNetwork { network: String },

    #[error("pair {network}/{sdk} excludes scenario `{id}`, which is not declared")]
    UndeclaredExclusion {
        network: String,
        sdk: String,
        id: String,
    },
}

pub fn validate(manifest: &Manifest) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();

    for (position, scenario) in manifest.scenarios.iter().enumerate() {
        if scenario.id.trim().is_empty() {
            errors.push(ValidationError::EmptyId { position });
            continue;
        }
        if !seen.insert(scenario.id.as_str()) {
            errors.push(ValidationError::DuplicateId {
                id: scenario.id.clone(),
            });
        }

        for action in &scenario.actions {
            if let Action::Unknown(name) = action {
                errors.push(ValidationError::UnknownAction {
                    id: scenario.id.clone(),
                    action: name.clone(),
                });
            }
        }

        if scenario.actions.contains(&Action::ProposalCreateOffline)
            && !scenario.actions.contains(&Action::GuardianMigrate)
        {
            errors.push(ValidationError::OfflineCreationOutsideMigration {
                id: scenario.id.clone(),
            });
        }

        if scenario.runtime == Some(Runtime::Browser) && scenario.sdk.includes(Sdk::Rust) {
            errors.push(ValidationError::BrowserRuntimeOnRustSdk {
                id: scenario.id.clone(),
            });
        }

        if scenario.sdk.includes(Sdk::Typescript) && scenario.runtime.is_none() {
            errors.push(ValidationError::MissingRuntime {
                id: scenario.id.clone(),
            });
        }

        if scenario.scheme == Scheme::Unrecognized {
            errors.push(ValidationError::UnrecognizedScheme {
                id: scenario.id.clone(),
            });
        }
        let mixed_actions = scenario
            .actions
            .iter()
            .filter(|a| a.is_mixed_scheme())
            .count();
        let labelled_mixed = scenario.scheme == Scheme::Mixed;
        if (labelled_mixed && mixed_actions != scenario.actions.len())
            || (!labelled_mixed && mixed_actions > 0)
        {
            errors.push(ValidationError::MixedSchemeActions {
                id: scenario.id.clone(),
            });
        }

        // The Rust driver builds a mixed-scheme account itself, from the
        // upstream component, and drives the server with it. Flows through an
        // SDK, which is every live scenario and the whole TypeScript leg,
        // cannot construct one yet.
        if scenario.scheme == Scheme::Mixed
            && (scenario.profile != Profile::Deterministic
                || scenario.sdk.includes(Sdk::Typescript))
        {
            errors.push(ValidationError::MixedScheme {
                id: scenario.id.clone(),
            });
        }
    }

    for pair in &manifest.pairs {
        let Some(network) = manifest.network(pair.network) else {
            errors.push(ValidationError::UndeclaredNetwork {
                network: pair.network.as_str().to_string(),
            });
            continue;
        };

        for excluded in &pair.excluded_scenarios {
            if manifest.scenario(excluded).is_none() {
                errors.push(ValidationError::UndeclaredExclusion {
                    network: pair.network.as_str().to_string(),
                    sdk: sdk_label(pair.sdk).to_string(),
                    id: excluded.clone(),
                });
            }
        }

        for scenario in manifest.required_scenarios(pair) {
            if pair.availability == Availability::Unavailable {
                errors.push(ValidationError::RequiredOnUnavailablePair {
                    id: scenario.id.clone(),
                    network: pair.network.as_str().to_string(),
                    sdk: sdk_label(pair.sdk).to_string(),
                });
                continue;
            }
            if scenario.step_budget > network.historical_window {
                errors.push(ValidationError::BudgetExceedsWindow {
                    id: scenario.id.clone(),
                    network: pair.network.as_str().to_string(),
                    budget_seconds: scenario.step_budget.as_seconds(),
                    window_seconds: network.historical_window.as_seconds(),
                });
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn sdk_label(sdk: Sdk) -> &'static str {
    match sdk {
        Sdk::Rust => "rust",
        Sdk::Typescript => "typescript",
    }
}

pub fn applies_to(selector: SdkSelector, sdk: Sdk) -> bool {
    selector.includes(sdk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    const NETWORKS: &str = r#"
[[network]]
name = "testnet"
rpc_endpoint = "https://rpc.testnet.miden.io"
historical_window = "30m"

[[network]]
name = "devnet"
rpc_endpoint = "https://rpc.devnet.miden.io"
historical_window = "150s"
"#;

    fn manifest(scenarios: &str, pairs: &str) -> Manifest {
        let matrix = format!("{NETWORKS}\n{pairs}");
        Manifest::from_toml(scenarios, &matrix).expect("manifest parses")
    }

    fn scenario(id: &str, extra: &str) -> String {
        format!(
            r#"
[[scenario]]
id = "{id}"
title = "t"
profile = "live"
sdk = "rust"
scheme = "falcon"
shape = "2-of-3"
mode = "online"
actions = ["proposal-create", "proposal-sign", "proposal-execute"]
step_budget = "90s"
required = false
{extra}
"#
        )
    }

    #[test]
    fn accepts_a_well_formed_manifest() {
        let m = manifest(&scenario("a", ""), "");
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn rejects_duplicate_ids() {
        let doc = format!("{}{}", scenario("a", ""), scenario("a", ""));
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::DuplicateId { id: "a".into() }));
    }

    #[test]
    fn rejects_offline_creation_outside_migration() {
        let doc = r#"
[[scenario]]
id = "bad-offline"
title = "t"
profile = "live"
sdk = "rust"
scheme = "falcon"
shape = "1-of-1"
mode = "offline"
actions = ["proposal-create-offline", "asset-transfer"]
step_budget = "90s"
required = false
"#;
        let errors = validate(&manifest(doc, "")).unwrap_err();
        assert!(
            errors.contains(&ValidationError::OfflineCreationOutsideMigration {
                id: "bad-offline".into()
            })
        );
    }

    #[test]
    fn accepts_offline_creation_for_guardian_migration() {
        let doc = r#"
[[scenario]]
id = "migrate-offline"
title = "t"
profile = "live"
sdk = "rust"
scheme = "ecdsa"
shape = "1-of-1"
mode = "offline"
actions = ["proposal-create-offline", "guardian-migrate", "proposal-execute"]
step_budget = "240s"
required = false
"#;
        assert!(validate(&manifest(doc, "")).is_ok());
    }

    #[test]
    fn rejects_browser_runtime_on_rust_sdk() {
        let doc = scenario("a", r#"runtime = "browser""#);
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::BrowserRuntimeOnRustSdk { id: "a".into() }));
    }

    #[test]
    fn rejects_mixed_scheme_on_the_live_profile() {
        let doc = scenario("a", "").replace(r#"scheme = "falcon""#, r#"scheme = "mixed""#);
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::MixedScheme { id: "a".into() }));
    }

    #[test]
    fn rejects_mixed_scheme_for_the_typescript_leg() {
        let doc = scenario("a", r#"runtime = "server-side""#)
            .replace(r#"profile = "live""#, r#"profile = "deterministic""#)
            .replace(r#"sdk = "rust""#, r#"sdk = "both""#)
            .replace(r#"scheme = "falcon""#, r#"scheme = "mixed""#);
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::MixedScheme { id: "a".into() }));
    }

    // Before `mixed` was allowed anywhere, every unknown spelling became
    // `Mixed` and was refused; now a misspelling on the deterministic Rust leg
    // would be accepted as mixed unless it is refused in its own right.
    #[test]
    fn rejects_an_unrecognized_scheme_even_where_mixed_is_allowed() {
        let doc = scenario("a", "")
            .replace(r#"profile = "live""#, r#"profile = "deterministic""#)
            .replace(r#"scheme = "falcon""#, r#"scheme = "falcon+ecdsa""#);
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::UnrecognizedScheme { id: "a".into() }));
        assert!(!errors.contains(&ValidationError::MixedScheme { id: "a".into() }));
    }

    fn deterministic_mixed(actions: &str) -> String {
        scenario("a", "")
            .replace(r#"profile = "live""#, r#"profile = "deterministic""#)
            .replace(r#"scheme = "falcon""#, r#"scheme = "mixed""#)
            .replace(
                r#"actions = ["proposal-create", "proposal-sign", "proposal-execute"]"#,
                actions,
            )
    }

    #[test]
    fn accepts_mixed_scheme_on_the_deterministic_rust_leg() {
        let doc =
            deterministic_mixed(r#"actions = ["mixed-scheme-register", "mixed-scheme-approvals"]"#);
        assert_eq!(validate(&manifest(&doc, "")), Ok(()));
    }

    #[test]
    fn rejects_a_mixed_label_on_actions_that_build_no_mixed_account() {
        let doc = deterministic_mixed(r#"actions = ["mixed-scheme-register", "account-register"]"#);
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::MixedSchemeActions { id: "a".into() }));
    }

    #[test]
    fn rejects_mixed_scheme_actions_under_a_single_scheme_label() {
        let doc = scenario("a", "")
            .replace(r#"profile = "live""#, r#"profile = "deterministic""#)
            .replace(
                r#"actions = ["proposal-create", "proposal-sign", "proposal-execute"]"#,
                r#"actions = ["mixed-scheme-register"]"#,
            );
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::MixedSchemeActions { id: "a".into() }));
    }

    #[test]
    fn rejects_unknown_action() {
        let doc = scenario("a", "").replace(
            r#"actions = ["proposal-create", "proposal-sign", "proposal-execute"]"#,
            r#"actions = ["teleport-funds"]"#,
        );
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::UnknownAction {
            id: "a".into(),
            action: "teleport-funds".into()
        }));
    }

    #[test]
    fn rejects_budget_exceeding_the_networks_window() {
        let doc = scenario("slow", "").replace(r#"step_budget = "90s""#, r#"step_budget = "10m""#);
        let doc = doc.replace("required = false", "required = true");
        let pairs = r#"
[[pair]]
network = "devnet"
sdk = "rust"
availability = "available"
"#;
        let errors = validate(&manifest(&doc, pairs)).unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::BudgetExceedsWindow { id, network, .. }
                if id == "slow" && network == "devnet"
        )));
    }

    #[test]
    fn accepts_a_long_scenario_when_the_pair_excludes_it() {
        let doc = scenario("slow", "")
            .replace(r#"step_budget = "90s""#, r#"step_budget = "10m""#)
            .replace("required = false", "required = true");
        let pairs = r#"
[[pair]]
network = "devnet"
sdk = "rust"
availability = "available"
excluded_scenarios = ["slow"]
"#;
        assert!(validate(&manifest(&doc, pairs)).is_ok());
    }

    #[test]
    fn rejects_a_required_scenario_on_an_unavailable_pair() {
        let doc = scenario("a", "").replace("required = false", "required = true");
        let pairs = r#"
[[pair]]
network = "devnet"
sdk = "rust"
availability = "unavailable"
reason = "gateway refuses this transport"
"#;
        let errors = validate(&manifest(&doc, pairs)).unwrap_err();
        assert!(
            errors.contains(&ValidationError::RequiredOnUnavailablePair {
                id: "a".into(),
                network: "devnet".into(),
                sdk: "rust".into()
            })
        );
    }

    #[test]
    fn rejects_an_exclusion_naming_an_undeclared_scenario() {
        let pairs = r#"
[[pair]]
network = "testnet"
sdk = "rust"
availability = "available"
excluded_scenarios = ["ghost"]
"#;
        let errors = validate(&manifest(&scenario("a", ""), pairs)).unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::UndeclaredExclusion { id, .. } if id == "ghost"
        )));
    }

    #[test]
    fn requires_a_runtime_for_typescript_targets() {
        let doc = scenario("a", "").replace(r#"sdk = "rust""#, r#"sdk = "typescript""#);
        let errors = validate(&manifest(&doc, "")).unwrap_err();
        assert!(errors.contains(&ValidationError::MissingRuntime { id: "a".into() }));
    }
}
