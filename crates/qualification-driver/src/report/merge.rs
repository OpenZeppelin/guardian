use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Conclusion, QualificationClaim, RunResult, ScenarioResult, derive};
use crate::manifest::{Manifest, NetworkName};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkOutcome {
    pub conclusion: Conclusion,
    pub qualification_claim: QualificationClaim,
    pub runs: Vec<RunResult>,
}

/// Per-network results stay separate. A summary may say whether every network
/// passed, but the individual outcomes remain visible: merging them into one
/// verdict lets a healthy network mask a broken one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedReport {
    pub networks: BTreeMap<String, NetworkOutcome>,
    pub every_network_passed: bool,
    pub every_network_fully_qualified: bool,
}

pub fn merge(runs: Vec<RunResult>) -> MergedReport {
    let mut grouped: BTreeMap<NetworkName, Vec<RunResult>> = BTreeMap::new();
    for run in runs {
        grouped.entry(run.network.name).or_default().push(run);
    }

    let mut networks = BTreeMap::new();
    for (name, runs) in grouped {
        let conclusion = if runs.iter().any(|r| r.conclusion == Conclusion::Failure) {
            Conclusion::Failure
        } else {
            Conclusion::Success
        };
        let qualification_claim = weakest_claim(&runs);
        networks.insert(
            name.as_str().to_string(),
            NetworkOutcome {
                conclusion,
                qualification_claim,
                runs,
            },
        );
    }

    let every_network_passed = networks
        .values()
        .all(|outcome| outcome.conclusion == Conclusion::Success);
    let every_network_fully_qualified = networks
        .values()
        .all(|outcome| outcome.qualification_claim == QualificationClaim::Full);

    MergedReport {
        networks,
        every_network_passed,
        every_network_fully_qualified,
    }
}

fn weakest_claim(runs: &[RunResult]) -> QualificationClaim {
    if runs
        .iter()
        .any(|r| r.qualification_claim == QualificationClaim::None)
    {
        return QualificationClaim::None;
    }
    if runs
        .iter()
        .any(|r| r.qualification_claim == QualificationClaim::Partial)
    {
        return QualificationClaim::Partial;
    }
    QualificationClaim::Full
}

/// One SDK's scenario results, without the run-level fields.
///
/// The TypeScript driver emits this rather than a whole run: conclusion and
/// qualification claim are derived from the required set, and that derivation
/// lives here. Duplicating it there is how the two sides start disagreeing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialRun {
    pub run_id: String,
    pub scenario_results: Vec<super::ScenarioResult>,
}

/// Merges the run results in `directory`.
///
/// `run_id` narrows it to one run. The directory is shared across runs, so a
/// run whose Rust leg never wrote a result leaves its TypeScript results
/// orphaned there, and an orphan is fatal by design: results without their run
/// mean a leg died. Without the filter that verdict lands on whichever run
/// merges next, failing a run that was fine. Left unset, every file is
/// considered, which is what a standalone merge across runs wants.
pub fn merge_directory(
    directory: &Path,
    manifest: Option<&Manifest>,
    run_id: Option<&str>,
) -> anyhow::Result<MergedReport> {
    let mut runs: Vec<RunResult> = Vec::new();
    let mut partials: Vec<PartialRun> = Vec::new();

    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if !path.extension().is_some_and(|ext| ext == "json") {
            continue;
        }
        let raw = std::fs::read_to_string(&path)?;
        match serde_json::from_str::<RunResult>(&raw) {
            Ok(run) => {
                if run_id.is_none_or(|wanted| run.run_id == wanted) {
                    runs.push(run);
                }
            }
            Err(run_error) => match serde_json::from_str::<PartialRun>(&raw) {
                Ok(partial) => {
                    if run_id.is_none_or(|wanted| partial.run_id == wanted) {
                        partials.push(partial);
                    }
                }
                Err(_) => {
                    anyhow::bail!(
                        "{} is neither a run nor an SDK's results: {run_error}",
                        path.display()
                    )
                }
            },
        }
    }

    for partial in partials {
        // The run whose id this belongs to, so one run carries both SDKs'
        // results and the claim is derived once over the whole set.
        let Some(run) = runs.iter_mut().find(|run| run.run_id == partial.run_id) else {
            anyhow::bail!(
                "results for run {} arrived without the run itself",
                partial.run_id
            );
        };
        run.scenario_results.extend(partial.scenario_results);
    }

    // Each leg derived its verdict over its own SDK's required set, so a run
    // that has just absorbed the other SDK's results is carrying a claim that
    // never saw them. Recomputed over everything, or a Rust-only "full" would
    // survive a TypeScript failure.
    if let Some(manifest) = manifest {
        for run in &mut runs {
            restate(run, manifest);
        }
    }

    Ok(merge(runs))
}

fn restate(run: &mut RunResult, manifest: &Manifest) {
    run.conclusion = derive::conclusion(&run.scenario_results);

    let profile = run.scenario_results.iter().find_map(|result| {
        manifest
            .scenarios
            .iter()
            .find(|scenario| scenario.id == result.scenario_id)
            .map(|scenario| scenario.profile)
    });
    let Some(profile) = profile else {
        return;
    };

    let required = manifest.required_entries(profile, Some(run.network.name), None);
    let recomputed = derive::qualification_claim(&run.scenario_results, &required, false);
    // Weaker of the two: a filtered run already claims nothing, and recomputing
    // must never talk it back up.
    run.qualification_claim = weakest_claim_of([run.qualification_claim, recomputed]);

    let operator_covered = run
        .scenario_results
        .iter()
        .filter(|result| result.scenario_id.contains("operator"))
        .any(ScenarioResult::counts_as_pass);
    run.not_covered = derive::not_covered(operator_covered);
}

fn weakest_claim_of(claims: [QualificationClaim; 2]) -> QualificationClaim {
    if claims.contains(&QualificationClaim::None) {
        return QualificationClaim::None;
    }
    if claims.contains(&QualificationClaim::Partial) {
        return QualificationClaim::Partial;
    }
    QualificationClaim::Full
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duration::Budget;
    use crate::manifest::{Runtime, Sdk};
    use crate::report::emit;
    use crate::report::{ArtifactSet, FundingSummary, NetworkSummary, Outcome, Pairing, Trigger};
    use chrono::Utc;

    /// The shape the TypeScript emitter actually writes, pinned here so the two
    /// sides cannot drift apart silently.
    #[test]
    fn an_sdks_results_fold_into_the_run_that_carries_them() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut run = run(
            NetworkName::Testnet,
            Conclusion::Success,
            QualificationClaim::Partial,
        );
        run.run_id = "run-1".to_string();
        run.scenario_results.clear();
        emit::write(&run, directory.path()).expect("writes the run");

        std::fs::write(
            directory.path().join("run-1-typescript.json"),
            serde_json::json!({
                "run_id": "run-1",
                "scenario_results": [{
                    "scenario_id": "live-register-falcon-1of1",
                    "sdk": "typescript",
                    "runtime": "server-side",
                    "outcome": "passed",
                    "embedded_retry": true,
                    "duration": "PT1S",
                }],
            })
            .to_string(),
        )
        .expect("writes the results");

        let merged = merge_directory(directory.path(), None, None).expect("merges");
        let runs = &merged.networks.values().next().expect("one network").runs;
        assert_eq!(
            runs.len(),
            1,
            "the results joined the run, not stood beside it"
        );
        assert_eq!(runs[0].scenario_results.len(), 1);
        assert_eq!(runs[0].scenario_results[0].sdk, Sdk::Typescript);
        assert_eq!(runs[0].scenario_results[0].runtime, Runtime::ServerSide);
        assert_eq!(runs[0].scenario_results[0].outcome, Outcome::Passed);
    }

    /// The Rust leg derives its claim over Rust's required set alone, so a
    /// TypeScript failure folded in afterwards must take the claim down with it.
    #[test]
    fn a_folded_in_failure_restates_the_claim() {
        let directory = tempfile::tempdir().expect("tempdir");
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("qualification/manifest");
        let manifest = Manifest::load(
            &manifest_dir.join("scenarios.toml"),
            &manifest_dir.join("matrix.toml"),
        )
        .expect("the committed manifest loads");

        let mut run = run(
            NetworkName::Testnet,
            Conclusion::Success,
            QualificationClaim::Full,
        );
        run.run_id = "run-2".to_string();
        run.scenario_results.clear();
        emit::write(&run, directory.path()).expect("writes the run");

        std::fs::write(
            directory.path().join("run-2-typescript.json"),
            serde_json::json!({
                "run_id": "run-2",
                "scenario_results": [{
                    "scenario_id": "det-fixture-http",
                    "sdk": "typescript",
                    "runtime": "server-side",
                    "outcome": "failed",
                    "reason": "deliberate",
                    "classification": "product",
                    "embedded_retry": true,
                    "duration": "PT1S",
                }],
            })
            .to_string(),
        )
        .expect("writes the results");

        let merged = merge_directory(directory.path(), Some(&manifest), None).expect("merges");
        let run = &merged.networks.values().next().expect("one network").runs[0];
        assert_eq!(run.conclusion, Conclusion::Failure);
        assert_ne!(
            run.qualification_claim,
            QualificationClaim::Full,
            "a full claim survived a folded-in failure"
        );
    }

    #[test]
    fn results_without_their_run_are_refused() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("orphan-typescript.json"),
            serde_json::json!({"run_id": "missing", "scenario_results": []}).to_string(),
        )
        .expect("writes the results");

        let error = merge_directory(directory.path(), None, None).expect_err("refuses");
        assert!(
            error.to_string().contains("without the run itself"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_earlier_runs_orphan_does_not_fail_this_runs_merge() {
        // The results directory is shared. A run whose Rust leg died leaves its
        // TypeScript results behind, and without the filter that orphan fails
        // every later merge, reporting a setup failure for a run that was fine.
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            directory.path().join("dead-run-typescript.json"),
            serde_json::json!({"run_id": "dead-run", "scenario_results": []}).to_string(),
        )
        .expect("writes the orphan");

        let mine = run(
            NetworkName::Testnet,
            Conclusion::Success,
            QualificationClaim::Full,
        );
        let my_id = mine.run_id.clone();
        std::fs::write(
            directory.path().join("mine.json"),
            serde_json::to_string(&mine).expect("serializes"),
        )
        .expect("writes the run");

        let merged =
            merge_directory(directory.path(), None, Some(&my_id)).expect("merges this run alone");
        assert_eq!(merged.networks.len(), 1);
        assert!(merged.every_network_passed);

        // Unfiltered it is still fatal: results without their run mean a leg died.
        assert!(merge_directory(directory.path(), None, None).is_err());
    }

    fn run(network: NetworkName, conclusion: Conclusion, claim: QualificationClaim) -> RunResult {
        RunResult {
            run_id: format!("{}-{:?}", network.as_str(), claim),
            started_at: Utc::now(),
            trigger: Trigger::Schedule,
            requested_by: None,
            network: NetworkSummary {
                name: network,
                observed_protocol_version: None,
                historical_window: Budget::from_seconds(1800),
                fee_asset: None,
            },
            artifact_set: ArtifactSet {
                image_digest: format!("sha256:{}", "a".repeat(64)),
                image_revision: "abc".into(),
                pairing: Pairing::Branch,
                sdk_versions: BTreeMap::new(),
                sdk_integrity: BTreeMap::new(),
                miden_versions: BTreeMap::new(),
            },
            scenario_results: Vec::new(),
            funding_summary: FundingSummary::not_required(),
            conclusion,
            qualification_claim: claim,
            not_covered: Vec::new(),
            consumer_findings: Vec::new(),
        }
    }

    #[test]
    fn networks_keep_separate_outcomes() {
        let merged = merge(vec![
            run(
                NetworkName::Testnet,
                Conclusion::Success,
                QualificationClaim::Full,
            ),
            run(
                NetworkName::Devnet,
                Conclusion::Failure,
                QualificationClaim::Partial,
            ),
        ]);
        assert_eq!(merged.networks.len(), 2);
        assert_eq!(merged.networks["testnet"].conclusion, Conclusion::Success);
        assert_eq!(merged.networks["devnet"].conclusion, Conclusion::Failure);
        assert!(!merged.every_network_passed);
    }

    #[test]
    fn a_healthy_network_does_not_mask_a_broken_one() {
        let merged = merge(vec![
            run(
                NetworkName::Testnet,
                Conclusion::Success,
                QualificationClaim::Full,
            ),
            run(
                NetworkName::Devnet,
                Conclusion::Failure,
                QualificationClaim::Partial,
            ),
        ]);
        assert!(!merged.every_network_passed);
        assert!(!merged.every_network_fully_qualified);
    }

    #[test]
    fn the_weakest_claim_wins_within_a_network() {
        let merged = merge(vec![
            run(
                NetworkName::Testnet,
                Conclusion::Success,
                QualificationClaim::Full,
            ),
            run(
                NetworkName::Testnet,
                Conclusion::Success,
                QualificationClaim::Partial,
            ),
        ]);
        assert_eq!(
            merged.networks["testnet"].qualification_claim,
            QualificationClaim::Partial
        );
    }
}
