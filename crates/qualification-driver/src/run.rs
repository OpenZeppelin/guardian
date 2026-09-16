use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::duration::Budget;
use crate::manifest::{Manifest, NetworkName, Profile, Scenario, Sdk, validate};
use crate::report::{
    ArtifactSet, Classification, Conclusion, FundingSummary, NetworkSummary, Outcome, RunResult,
    ScenarioResult, Trigger, derive, emit,
};
use crate::scenario::{Endpoints, Expectation, Runner};

pub struct RunOptions {
    pub profile: Profile,
    pub network: Option<NetworkName>,
    pub sdk: Option<Sdk>,
    pub scenarios: Vec<String>,
    pub core_only: bool,
    pub filtered: bool,
    pub post_restart: bool,
    pub account_dir: PathBuf,
    pub run_id: String,
    pub trigger: Trigger,
    pub requested_by: Option<String>,
    pub endpoints: Endpoints,
    pub artifact_set: ArtifactSet,
    pub out: PathBuf,
}

pub const EXIT_SUCCESS: i32 = 0;
pub const EXIT_PRODUCT_FAILURE: i32 = 1;
pub const EXIT_SETUP_FAILURE: i32 = 2;
pub const EXIT_ENVIRONMENT_BLOCKED: i32 = 3;

pub async fn execute(manifest_dir: &Path, options: RunOptions) -> anyhow::Result<(RunResult, i32)> {
    let manifest = Manifest::load(
        &manifest_dir.join("scenarios.toml"),
        &manifest_dir.join("matrix.toml"),
    )?;
    if let Err(errors) = validate::validate(&manifest) {
        for error in &errors {
            eprintln!("error: {error}");
        }
        anyhow::bail!("manifest validation failed");
    }

    let selected = select(&manifest, &options);
    let repo_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let fixtures = match crate::fixtures::Fixtures::load(&repo_root) {
        Ok(fixtures) => Some(fixtures),
        Err(error) => {
            eprintln!(
                "warning: server fixtures unavailable ({error}); fixture scenarios will fail"
            );
            None
        }
    };

    let runner = Runner::new(
        Endpoints {
            http: options.endpoints.http.clone(),
            grpc: options.endpoints.grpc.clone(),
        },
        Expectation {
            image_revision: options.artifact_set.image_revision.clone(),
        },
    )?
    .with_fixtures(fixtures)
    .post_restart(options.post_restart)
    .with_live(options.network.map(|network| {
        crate::scenario::live::LiveContext {
            network,
            // The multisig SDK speaks gRPC to GUARDIAN, not HTTP.
            guardian_endpoint: options.endpoints.grpc.clone(),
            account_dir: options.account_dir.clone(),
            // gRPC, not HTTP: this driver reaches GUARDIAN over gRPC, and the
            // HTTP listener cannot answer it.
            migration_endpoint: std::env::var("QUAL_GUARDIAN_MIGRATION_GRPC")
                .ok()
                .filter(|endpoint| !endpoint.trim().is_empty()),
        }
    }));

    let mut results: Vec<ScenarioResult> = Vec::new();
    for (scenario, sdk) in selected {
        let result = runner.run(scenario, sdk).await;
        println!(
            "  {:<44} {:<11} {:?}",
            scenario.id,
            format!("{sdk:?}").to_lowercase(),
            result.outcome
        );
        if let Some(reason) = &result.reason {
            println!("      {reason}");
        }
        results.push(result);
    }

    let required = required_entries(&manifest, &options);
    let conclusion = derive::conclusion(&results);
    let claim = derive::qualification_claim(&results, &required, options.filtered);
    let operator_covered = results
        .iter()
        .filter(|result| result.scenario_id.contains("operator"))
        .any(ScenarioResult::counts_as_pass);

    let network = options.network.unwrap_or(NetworkName::Testnet);
    let window = manifest
        .network(network)
        .map(|entry| entry.historical_window)
        .unwrap_or_else(|| Budget::from_seconds(0));

    let result = RunResult {
        run_id: options.run_id.clone(),
        started_at: Utc::now(),
        trigger: options.trigger,
        requested_by: options.requested_by.clone(),
        network: NetworkSummary {
            name: network,
            observed_protocol_version: None,
            historical_window: window,
            fee_asset: None,
        },
        artifact_set: options.artifact_set.clone(),
        scenario_results: results,
        funding_summary: funding_summary(&options),
        conclusion,
        qualification_claim: claim,
        not_covered: derive::not_covered(operator_covered),
        consumer_findings: Vec::new(),
    };

    if let Err(errors) = result.validate() {
        for error in &errors {
            eprintln!("error: {error}");
        }
        anyhow::bail!("the run result is internally inconsistent");
    }

    emit::write(&result, &options.out)?;
    let code = exit_code(&result);
    Ok((result, code))
}

/// What this run spent, recorded rather than assumed.
///
/// The deterministic profile touches no treasury, so `not_required` is the truth
/// there. A live run does spend, and reporting `not_required` for it made the
/// result claim something the run itself had just disproved.
fn funding_summary(options: &RunOptions) -> FundingSummary {
    if options.profile != Profile::Live || options.network.is_none() {
        return FundingSummary::not_required();
    }
    FundingSummary::spent(crate::funding::service::spent_so_far())
}

fn exit_code(result: &RunResult) -> i32 {
    if result.conclusion == Conclusion::Failure {
        let setup_only = result
            .scenario_results
            .iter()
            .filter(|entry| entry.outcome == Outcome::Failed)
            .all(|entry| entry.classification == Some(Classification::Setup));
        return if setup_only {
            EXIT_SETUP_FAILURE
        } else {
            EXIT_PRODUCT_FAILURE
        };
    }
    let ran = !result.scenario_results.is_empty();
    let all_blocked = ran
        && result
            .scenario_results
            .iter()
            .all(|entry| entry.outcome == Outcome::EnvironmentBlocked);
    if all_blocked {
        EXIT_ENVIRONMENT_BLOCKED
    } else {
        EXIT_SUCCESS
    }
}

fn select<'a>(manifest: &'a Manifest, options: &RunOptions) -> Vec<(&'a Scenario, Sdk)> {
    let mut selected = Vec::new();
    for scenario in &manifest.scenarios {
        if scenario.profile != options.profile {
            continue;
        }
        if !options.scenarios.is_empty() && !options.scenarios.contains(&scenario.id) {
            continue;
        }
        if options.core_only && !scenario.core {
            continue;
        }
        for sdk in scenario.sdk.expand() {
            if let Some(requested) = options.sdk
                && requested != sdk
            {
                continue;
            }
            selected.push((scenario, sdk));
        }
    }
    selected
}

fn required_entries(manifest: &Manifest, options: &RunOptions) -> Vec<derive::RequiredEntry> {
    manifest.required_entries(options.profile, options.network, options.sdk)
}
