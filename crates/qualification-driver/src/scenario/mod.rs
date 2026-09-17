pub mod account;
pub mod error_envelope;
pub mod identity;
pub mod live;

use std::time::Instant;

use crate::duration::Budget;
use crate::fixtures::Fixtures;
use crate::manifest::{Action, Runtime, Scenario, Sdk};
use crate::report::{Classification, Outcome, ScenarioResult};

pub struct Endpoints {
    pub http: String,
    pub grpc: String,
}

pub struct Expectation {
    pub image_revision: String,
}

/// The outcome of one action, before it is folded into a scenario result.
pub enum ActionOutcome {
    Passed,
    Failed {
        reason: String,
        classification: Classification,
    },
    Skipped {
        reason: String,
    },
    EnvironmentBlocked {
        reason: String,
    },
}

impl ActionOutcome {
    pub fn failed_product(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
            classification: Classification::Product,
        }
    }

    pub fn failed_setup(reason: impl Into<String>) -> Self {
        Self::Failed {
            reason: reason.into(),
            classification: Classification::Setup,
        }
    }
}

pub struct Runner {
    pub endpoints: Endpoints,
    pub expectation: Expectation,
    pub http: reqwest::Client,
    pub fixtures: Option<Fixtures>,
    pub post_restart: bool,
    pub live: Option<live::LiveContext>,
    /// Scoped to one scenario: its actions share the account they act on, and
    /// it is cleared between scenarios so no run can inherit another's state.
    pub session: tokio::sync::Mutex<Option<live::LiveSession>>,
}

impl Runner {
    pub fn new(endpoints: Endpoints, expectation: Expectation) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            // The operator dashboard authenticates with a session cookie, so a
            // client without a store logs in and is then refused on the request
            // the login was for.
            .cookie_store(true)
            .build()?;
        Ok(Self {
            endpoints,
            expectation,
            http,
            fixtures: None,
            post_restart: false,
            live: None,
            session: tokio::sync::Mutex::new(None),
        })
    }

    pub fn with_fixtures(mut self, fixtures: Option<Fixtures>) -> Self {
        self.fixtures = fixtures;
        self
    }

    pub fn post_restart(mut self, post_restart: bool) -> Self {
        self.post_restart = post_restart;
        self
    }

    pub fn with_live(mut self, live: Option<live::LiveContext>) -> Self {
        self.live = live;
        self
    }

    pub async fn run(&self, scenario: &Scenario, sdk: Sdk) -> ScenarioResult {
        let started = Instant::now();
        *self.session.lock().await = None;
        let mut outcome = ActionOutcome::Passed;

        for action in &scenario.actions {
            outcome = self.run_action(action, sdk, scenario, &scenario.id).await;
            if !matches!(outcome, ActionOutcome::Passed) {
                break;
            }
        }

        let duration = Budget::from_seconds(started.elapsed().as_secs());
        let runtime = scenario.runtime.unwrap_or(match sdk {
            Sdk::Rust => Runtime::Native,
            Sdk::Typescript => Runtime::ServerSide,
        });

        let (outcome, reason, classification) = match outcome {
            ActionOutcome::Passed => (Outcome::Passed, None, None),
            ActionOutcome::Failed {
                reason,
                classification,
            } => (Outcome::Failed, Some(reason), Some(classification)),
            ActionOutcome::Skipped { reason } => (Outcome::Skipped, Some(reason), None),
            ActionOutcome::EnvironmentBlocked { reason } => {
                (Outcome::EnvironmentBlocked, Some(reason), None)
            }
        };

        ScenarioResult {
            scenario_id: scenario.id.clone(),
            sdk,
            runtime,
            outcome,
            reason,
            classification,
            // The Rust path disables its transport retry loop and never retries
            // submissions; the TypeScript bundled client does, below the level
            // this project controls.
            embedded_retry: sdk == Sdk::Typescript,
            duration,
        }
    }

    /// An unwritten action on a required scenario fails rather than skips.
    /// Skips do not fail a run, so skipping here would let a gate report green
    /// for coverage that does not exist.
    async fn run_action(
        &self,
        action: &Action,
        sdk: Sdk,
        scenario: &Scenario,
        run_tag: &str,
    ) -> ActionOutcome {
        let required = scenario.required;
        let live_profile = scenario.profile == crate::manifest::Profile::Live;
        // This binary drives the Rust SDK. Satisfying a TypeScript-targeted
        // scenario with a Rust implementation would report TypeScript coverage
        // that no TypeScript code was involved in producing.
        if sdk == Sdk::Typescript {
            return if required {
                ActionOutcome::failed_setup(
                    "this entry point drives the Rust SDK only; the TypeScript driver is not \
                     invoked from here, so its coverage cannot be reported",
                )
            } else {
                ActionOutcome::Skipped {
                    reason: "the TypeScript driver is not invoked from this entry point"
                        .to_string(),
                }
            };
        }

        // Separate tables per profile. A shared one let a live scenario fall
        // through to a fixture implementation and report coverage the live
        // path never produced, twice.
        let handled = if live_profile {
            match action {
                Action::AccountHeritage => {
                    Some(live::open_heritage(self, scenario.scheme, run_tag).await)
                }
                Action::AccountCreate => {
                    Some(live::create(self, scenario.shape, scenario.scheme, run_tag).await)
                }
                Action::AccountRegister => Some(live::register(self).await),
                Action::CommitmentVerify => Some(live::verify_registration(self).await),
                Action::ProposalCreate => Some(live::create_proposal(self).await),
                Action::ProposalSign => Some(live::sign_proposal(self).await),
                Action::ProposalExecute => Some(live::execute_proposal(self).await),
                Action::ProposalRejectBelowThreshold => {
                    Some(live::reject_below_threshold(self).await)
                }
                Action::ProposalRejectDuplicateSignature => {
                    Some(live::reject_duplicate_signature(self).await)
                }
                Action::AccountRecoverByCosigner => Some(live::recover_by_cosigner(self).await),
                Action::BalanceAssert => Some(live::assert_balance(self).await),
                Action::AssetTransfer => Some(live::transfer_asset(self).await),
                Action::NoteConsume => Some(live::consume_note(self).await),
                Action::ProposalExport => Some(live::export_proposal(self).await),
                Action::ProposalSignExternal => Some(live::sign_proposal_external(self).await),
                Action::ProposalImport => Some(live::import_proposal(self).await),
                Action::ProposalCreateOffline => Some(live::create_proposal_offline(self).await),
                Action::GuardianMigrate => Some(live::assert_guardian_migration(self).await),
                Action::HandoffRustToTs => Some(live::handoff_to_typescript(self).await),
                Action::SignerAdd => Some(live::add_signer(self, run_tag).await),
                Action::SignerRemove => Some(live::remove_signer(self).await),
                Action::ThresholdChange => Some(live::change_threshold(self).await),
                Action::SignerSetAssert => Some(live::assert_signer_set(self).await),
                Action::SignerRemovedRefused => {
                    Some(live::assert_removed_signer_refused(self).await)
                }
                Action::AssetSend => Some(live::send_asset(self).await),
                Action::AssetSendAssert => Some(live::assert_asset_sent(self).await),
                Action::ProcedureThresholdSet => Some(live::set_procedure_threshold(self).await),
                Action::ProcedureThresholdAssert => {
                    Some(live::assert_procedure_threshold(self).await)
                }
                _ => None,
            }
        } else {
            match action {
                Action::StatusIdentity => Some(identity::assert_identity(self).await),
                Action::ErrorEnvelope => Some(error_envelope::assert_grpc_envelope(self).await),
                Action::AccountRegister => Some(account::register(self).await),
                Action::CommitmentVerify => Some(account::verify_commitment(self).await),
                Action::ProposalCreate => Some(account::create_proposal(self).await),
                Action::RestartDurability => Some(account::assert_durability(self).await),
                Action::SchemeGate => Some(account::assert_scheme_gate(self).await),
                Action::AccountPausedRefuses => {
                    Some(account::assert_paused_account_refuses(self).await)
                }
                _ => None,
            }
        };

        if let Some(outcome) = handled {
            return outcome;
        }

        match action {
            other if required => ActionOutcome::failed_setup(format!(
                "action {other:?} is required but has no driver implementation"
            )),
            other => ActionOutcome::Skipped {
                reason: format!("no driver implementation yet for action {other:?}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scenario(required: bool) -> Scenario {
        Scenario {
            id: "probe".to_string(),
            title: "probe".to_string(),
            profile: crate::manifest::Profile::Deterministic,
            sdk: crate::manifest::SdkSelector::Both,
            runtime: Some(Runtime::ServerSide),
            scheme: crate::manifest::Scheme::NotApplicable,
            shape: crate::manifest::Shape::NotApplicable,
            mode: crate::manifest::Mode::NotApplicable,
            actions: Vec::new(),
            step_budget: crate::duration::Budget::from_seconds(1),
            required,
            core: false,
        }
    }

    /// Uses an action that is still unimplemented; repoint it when the operator
    /// scenarios land.
    ///
    /// A required scenario whose action has no implementation must fail, not
    /// skip. Skips do not fail a run, so the opposite would let a merge gate
    /// report green for coverage that does not exist.
    /// A Rust implementation must never be recorded as TypeScript coverage.
    #[tokio::test]
    async fn a_typescript_leg_is_not_satisfied_by_the_rust_driver() {
        let runner = Runner::new(
            Endpoints {
                http: "http://127.0.0.1:1".to_string(),
                grpc: "http://127.0.0.1:1".to_string(),
            },
            Expectation {
                image_revision: String::new(),
            },
        )
        .expect("runner builds");

        let required = runner
            .run_action(
                &Action::ErrorEnvelope,
                Sdk::Typescript,
                &scenario(true),
                "probe",
            )
            .await;
        assert!(matches!(
            required,
            ActionOutcome::Failed {
                classification: Classification::Setup,
                ..
            }
        ));

        let optional = runner
            .run_action(
                &Action::ErrorEnvelope,
                Sdk::Typescript,
                &scenario(false),
                "probe",
            )
            .await;
        assert!(matches!(optional, ActionOutcome::Skipped { .. }));
    }

    #[tokio::test]
    async fn an_unimplemented_action_fails_a_required_scenario() {
        let runner = Runner::new(
            Endpoints {
                http: "http://127.0.0.1:1".to_string(),
                grpc: "http://127.0.0.1:1".to_string(),
            },
            Expectation {
                image_revision: String::new(),
            },
        )
        .expect("runner builds");

        let outcome = runner
            .run_action(&Action::OperatorAudit, Sdk::Rust, &scenario(true), "probe")
            .await;
        assert!(matches!(
            outcome,
            ActionOutcome::Failed {
                classification: Classification::Setup,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn an_unimplemented_action_only_skips_an_optional_scenario() {
        let runner = Runner::new(
            Endpoints {
                http: "http://127.0.0.1:1".to_string(),
                grpc: "http://127.0.0.1:1".to_string(),
            },
            Expectation {
                image_revision: String::new(),
            },
        )
        .expect("runner builds");

        let outcome = runner
            .run_action(&Action::OperatorAudit, Sdk::Rust, &scenario(false), "probe")
            .await;
        assert!(matches!(outcome, ActionOutcome::Skipped { .. }));
    }
}
