pub mod artifacts;
pub mod derive;
pub mod emit;
pub mod findings;
pub mod merge;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::duration::Budget;
use crate::manifest::{NetworkName, Runtime, Sdk};

pub use artifacts::{ArtifactSet, Pairing};
pub use derive::{Conclusion, QualificationClaim};
pub use findings::{ConsumerFinding, typescript_consumer_findings};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
    EnvironmentBlocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Classification {
    Product,
    Environment,
    Setup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Schedule,
    Dispatch,
    PullRequest,
    Publication,
    PreRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario_id: String,
    pub sdk: Sdk,
    pub runtime: Runtime,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classification: Option<Classification>,
    pub embedded_retry: bool,
    pub duration: Budget,
}

impl ScenarioResult {
    pub fn counts_as_pass(&self) -> bool {
        self.outcome == Outcome::Passed
    }

    pub fn blocks_conclusion(&self) -> bool {
        self.outcome == Outcome::Failed
            && matches!(
                self.classification,
                Some(Classification::Product) | Some(Classification::Setup)
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSummary {
    pub name: NetworkName,
    pub observed_protocol_version: Option<String>,
    pub historical_window: Budget,
    pub fee_asset: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FundingSummary {
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub balance_at_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projected_remaining_runs: Option<u64>,
}

impl FundingSummary {
    pub fn not_required() -> Self {
        Self {
            required: false,
            balance_at_start: None,
            spent: None,
            projected_remaining_runs: None,
        }
    }

    /// A run that funded from the treasury, reporting what it transferred.
    ///
    /// The opening balance and the depletion projection come from the
    /// `treasury-check` preflight, which runs in its own process, so they are
    /// absent here rather than guessed. What this run spent is known exactly.
    pub fn spent(amount: u64) -> Self {
        Self {
            required: true,
            balance_at_start: None,
            spent: Some(amount.to_string()),
            projected_remaining_runs: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunResult {
    pub run_id: String,
    pub started_at: DateTime<Utc>,
    pub trigger: Trigger,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    pub network: NetworkSummary,
    pub artifact_set: ArtifactSet,
    pub scenario_results: Vec<ScenarioResult>,
    pub funding_summary: FundingSummary,
    pub conclusion: Conclusion,
    pub qualification_claim: QualificationClaim,
    pub not_covered: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumer_findings: Vec<ConsumerFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResultError {
    #[error("scenario `{scenario_id}` reports `{outcome}` without a reason")]
    MissingReason {
        scenario_id: String,
        outcome: String,
    },
    #[error("scenario `{scenario_id}` failed without a classification")]
    MissingClassification { scenario_id: String },
    #[error("a pull-request run must record who requested it")]
    MissingRequester,
}

impl RunResult {
    pub fn validate(&self) -> Result<(), Vec<ResultError>> {
        let mut errors = Vec::new();
        for result in &self.scenario_results {
            if result.outcome != Outcome::Passed && result.reason.is_none() {
                errors.push(ResultError::MissingReason {
                    scenario_id: result.scenario_id.clone(),
                    outcome: format!("{:?}", result.outcome),
                });
            }
            if result.outcome == Outcome::Failed && result.classification.is_none() {
                errors.push(ResultError::MissingClassification {
                    scenario_id: result.scenario_id.clone(),
                });
            }
        }
        if self.trigger == Trigger::PullRequest && self.requested_by.is_none() {
            errors.push(ResultError::MissingRequester);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}
