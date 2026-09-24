use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{Conclusion, Outcome, QualificationClaim, RunResult, ScenarioResult, derive};
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

/// How much a result weighs when two passes disagree about the same scenario.
///
/// `Skipped` sits below everything because it is the absence of a verdict, so
/// any later pass overrides it. Above that the order is the one the report
/// already uses: a failure outranks not being able to test, which outranks a
/// pass.
fn severity(result: &ScenarioResult) -> u8 {
    match result.outcome {
        Outcome::Skipped => 0,
        Outcome::Passed => 1,
        Outcome::EnvironmentBlocked => 2,
        Outcome::Failed => 3,
    }
}

/// Merges the run results in `directory`.
///
/// `run_id` narrows it to one run. The directory is shared across runs, so a
/// run whose Rust leg never wrote a result leaves its TypeScript results
/// orphaned there, and an orphan is fatal by design: results without their run
/// mean a leg died. Without the filter that verdict lands on whichever run
/// merges next, failing a run that was fine. Left unset, every file is
/// considered, which is what a standalone merge across runs wants.
///
/// The `-post-restart` pass counts as the same run. Durability can only be
/// asserted once the process that wrote the data is gone, so that scenario
/// skips on the first pass and passes on the second. Merging only the first
/// leaves a required skip standing, which makes a full claim unreachable for
/// any run that includes it. Where two passes both reach a verdict, the worse
/// one stands: see [`severity`].
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
                if run_id.is_none_or(|wanted| belongs_to(&run.run_id, wanted)) {
                    runs.push(run);
                }
            }
            Err(run_error) => match serde_json::from_str::<PartialRun>(&raw) {
                Ok(partial) => {
                    if run_id.is_none_or(|wanted| belongs_to(&partial.run_id, wanted)) {
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

    /// Whether a result file belongs to the run being merged, including the extra
    /// passes the harness runs under a suffixed id.
    fn belongs_to(candidate: &str, wanted: &str) -> bool {
        candidate == wanted
            || candidate
                .strip_prefix(wanted)
                .is_some_and(|suffix| suffix.starts_with('-'))
    }

    // Fold the harness's extra passes into the run they belong to, rather than
    // reporting one run per pass. Each pass carries the same scenarios, and the
    // claim is derived once over the union: `restart-durability` skips on the
    // first pass and passes on the second, and only the union shows it passed.
    if let Some(wanted) = run_id {
        let (mut base, extra): (Vec<RunResult>, Vec<RunResult>) =
            runs.into_iter().partition(|run| run.run_id == wanted);
        if let Some(run) = base.first_mut() {
            // Every pass is its own assertion of the same scenario under a
            // different condition, so the merged result is the worst of them.
            //
            // Appending makes the set an OR: a scenario that passed before a
            // restart and failed after it would still read as passing, so a run
            // could report `Failure` while claiming `Full`. Replacing
            // unconditionally is the same mistake mirrored: a scenario that
            // failed before the restart and passed after it would read as
            // passing, while the shell keeps the failing exit code from the
            // first pass, so the report and the job disagree.
            //
            // `Skipped` is the exception, because it is not a verdict. The
            // durability assertion skips before the restart by design, and only
            // the later pass says anything about it.
            for pass in extra {
                for result in pass.scenario_results {
                    match run.scenario_results.iter_mut().find(|existing| {
                        existing.scenario_id == result.scenario_id && existing.sdk == result.sdk
                    }) {
                        Some(existing) => {
                            if severity(&result) >= severity(existing) {
                                *existing = result;
                            }
                        }
                        None => run.scenario_results.push(result),
                    }
                }
            }
            runs = base;
        } else {
            // No base run: the extra passes are all there is, and merging them
            // under another run's name would attribute them to a run that never
            // wrote a result.
            runs = extra;
        }
    }

    for partial in partials {
        // The run whose id this belongs to, so one run carries both SDKs'
        // results and the claim is derived once over the whole set.
        let Some(run) = runs
            .iter_mut()
            .find(|run| belongs_to(&partial.run_id, &run.run_id))
        else {
            anyhow::bail!(
                "results for run {} arrived without the run itself",
                partial.run_id
            );
        };
        run.scenario_results.extend(partial.scenario_results);
    }

    // Applied over the combined set, because the leg that carries these
    // workarounds is merged in above: the Rust run that wrote the base file
    // never has TypeScript results of its own.
    for run in &mut runs {
        if run
            .scenario_results
            .iter()
            .any(|result| result.sdk == crate::manifest::Sdk::Typescript)
        {
            run.consumer_findings = super::typescript_consumer_findings();
        }
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
    // A filtered run claims nothing and recomputing must never talk it back up.
    // Otherwise the recomputed claim stands, because the results it was computed
    // over are the whole run: both SDKs' legs and every extra pass, with the
    // latest outcome per scenario. Keeping the weaker of the two instead would
    // pin the run to its first leg's verdict, and `restart-durability` can only
    // pass on the second pass, so `Full` would be unreachable for any run that
    // includes it.
    run.qualification_claim = if run.qualification_claim == QualificationClaim::None {
        QualificationClaim::None
    } else {
        recomputed
    };

    let operator_covered = run
        .scenario_results
        .iter()
        .filter(|result| result.scenario_id.contains("operator"))
        .any(ScenarioResult::counts_as_pass);
    run.not_covered = derive::not_covered(operator_covered);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duration::Budget;
    use crate::manifest::{Runtime, Sdk};
    use crate::report::emit;
    use crate::report::{
        ArtifactSet, Classification, FundingSummary, NetworkSummary, Outcome, Pairing, Trigger,
    };
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

    // Durability can only be asserted after the process that wrote the data is
    // gone, so `det-restart-durability` is required and can only pass on the
    // second pass. Asserted against the committed manifest and on the claim
    // itself: folding the files without recomputing the claim leaves the run
    // pinned to the first pass's `Partial`, which is the state this test exists
    // to catch.
    #[test]
    fn the_post_restart_pass_makes_a_full_claim_reachable() {
        let directory = tempfile::tempdir().expect("tempdir");
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("qualification/manifest");
        let manifest = Manifest::load(
            &manifest_dir.join("scenarios.toml"),
            &manifest_dir.join("matrix.toml"),
        )
        .expect("the committed manifest loads");

        let required =
            manifest.required_entries(crate::manifest::Profile::Deterministic, None, None);
        assert!(
            required
                .iter()
                .any(|(id, _)| id == "det-restart-durability"),
            "this test is only meaningful while durability is required"
        );

        // Everything required passes on the first pass except durability, which
        // can only skip there.
        let mut first = run(
            NetworkName::Testnet,
            Conclusion::Success,
            QualificationClaim::Partial,
        );
        first.run_id = "run-3".to_string();
        first.scenario_results = required
            .iter()
            .map(|(id, sdk)| {
                let outcome = if id == "det-restart-durability" {
                    Outcome::Skipped
                } else {
                    Outcome::Passed
                };
                result_for(id, *sdk, outcome)
            })
            .collect();
        emit::write(&first, directory.path()).expect("writes the run");

        let mut second = first.clone();
        second.run_id = "run-3-post-restart".to_string();
        second.scenario_results = vec![result_for(
            "det-restart-durability",
            Sdk::Rust,
            Outcome::Passed,
        )];
        emit::write(&second, directory.path()).expect("writes the second pass");

        let merged =
            merge_directory(directory.path(), Some(&manifest), Some("run-3")).expect("merges");
        let outcome = merged.networks.values().next().expect("one network");
        assert_eq!(outcome.runs.len(), 1, "the two passes are one run");
        assert_eq!(
            outcome.runs[0].qualification_claim,
            QualificationClaim::Full,
            "durability passed on the second pass, so the run covered its required set"
        );
    }

    // The fold keeps the worst verdict, not a union: a scenario that passed
    // before a restart and failed after it must read as failed, or a run could
    // report a failure while claiming full coverage.
    #[test]
    fn a_later_pass_overturns_an_earlier_outcome() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut first = run(
            NetworkName::Testnet,
            Conclusion::Success,
            QualificationClaim::Partial,
        );
        first.run_id = "run-4".to_string();
        first.scenario_results = vec![result_for(
            "det-restart-durability",
            Sdk::Rust,
            Outcome::Passed,
        )];
        emit::write(&first, directory.path()).expect("writes");

        let mut second = first.clone();
        second.run_id = "run-4-post-restart".to_string();
        second.scenario_results = vec![result_for(
            "det-restart-durability",
            Sdk::Rust,
            Outcome::Failed,
        )];
        emit::write(&second, directory.path()).expect("writes");

        let merged = merge_directory(directory.path(), None, Some("run-4")).expect("merges");
        let outcome = merged.networks.values().next().expect("one network");
        assert_eq!(outcome.runs[0].scenario_results.len(), 1);
        assert_eq!(outcome.runs[0].scenario_results[0].outcome, Outcome::Failed);
    }

    /// The mirror of the case above, and the one the severity order exists for.
    /// A restart pass that passes must not erase a product failure the first
    /// pass recorded: the shell keeps the failing exit code across passes, so
    /// erasing it makes the report and the job disagree about the same run.
    #[test]
    fn a_later_pass_does_not_erase_an_earlier_failure() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut first = run(
            NetworkName::Testnet,
            Conclusion::Failure,
            QualificationClaim::Partial,
        );
        first.run_id = "run-5".to_string();
        first.scenario_results = vec![ScenarioResult {
            classification: Some(Classification::Product),
            reason: Some("the proposal was rejected".to_string()),
            ..result_for("det-proposal-lifecycle", Sdk::Rust, Outcome::Failed)
        }];
        emit::write(&first, directory.path()).expect("writes");

        let mut second = first.clone();
        second.run_id = "run-5-post-restart".to_string();
        second.scenario_results = vec![result_for(
            "det-proposal-lifecycle",
            Sdk::Rust,
            Outcome::Passed,
        )];
        emit::write(&second, directory.path()).expect("writes");

        let merged = merge_directory(directory.path(), None, Some("run-5")).expect("merges");
        let outcome = merged.networks.values().next().expect("one network");
        assert_eq!(outcome.runs[0].scenario_results.len(), 1);
        assert_eq!(
            outcome.runs[0].scenario_results[0].outcome,
            Outcome::Failed,
            "the first pass failed, and a later pass passing does not unfail it"
        );
        assert_eq!(
            outcome.runs[0].conclusion,
            Conclusion::Failure,
            "the run carries a product failure, so it concludes as one"
        );
    }

    /// Skipped is the absence of a verdict rather than a good one, which is what
    /// lets the durability assertion skip before the restart and still count
    /// once the later pass runs it.
    #[test]
    fn a_later_pass_replaces_a_skip_in_either_direction() {
        for later in [Outcome::Passed, Outcome::Failed] {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut first = run(
                NetworkName::Testnet,
                Conclusion::Success,
                QualificationClaim::Partial,
            );
            first.run_id = "run-6".to_string();
            first.scenario_results = vec![result_for(
                "det-restart-durability",
                Sdk::Rust,
                Outcome::Skipped,
            )];
            emit::write(&first, directory.path()).expect("writes");

            let mut second = first.clone();
            second.run_id = "run-6-post-restart".to_string();
            second.scenario_results = vec![result_for("det-restart-durability", Sdk::Rust, later)];
            emit::write(&second, directory.path()).expect("writes");

            let merged = merge_directory(directory.path(), None, Some("run-6")).expect("merges");
            let outcome = merged.networks.values().next().expect("one network");
            assert_eq!(outcome.runs[0].scenario_results[0].outcome, later);
        }
    }

    /// The workarounds are properties of the published artifact, so a merged
    /// report that carries a TypeScript leg must carry them too. Emitting an
    /// empty list reads as "a consumer needs nothing special", which is false.
    #[test]
    fn a_merged_report_with_a_typescript_leg_records_the_consumer_findings() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut base = run(
            NetworkName::Testnet,
            Conclusion::Success,
            QualificationClaim::Partial,
        );
        base.run_id = "run-7".to_string();
        base.scenario_results = vec![result_for(
            "det-status-identity",
            Sdk::Rust,
            Outcome::Passed,
        )];
        emit::write(&base, directory.path()).expect("writes");

        let partial = PartialRun {
            run_id: "run-7".to_string(),
            scenario_results: vec![result_for(
                "det-error-envelope",
                Sdk::Typescript,
                Outcome::Passed,
            )],
        };
        std::fs::write(
            directory.path().join("run-7-typescript.json"),
            serde_json::to_string(&partial).expect("serializes"),
        )
        .expect("writes the TypeScript leg");

        let merged = merge_directory(directory.path(), None, Some("run-7")).expect("merges");
        let outcome = merged.networks.values().next().expect("one network");
        assert!(
            !outcome.runs[0].consumer_findings.is_empty(),
            "a TypeScript leg ran, so the workarounds it needed belong in the report"
        );
    }

    /// A Rust-only run needs none of them, so recording them would overstate
    /// what the run touched.
    #[test]
    fn a_rust_only_report_records_no_consumer_findings() {
        let directory = tempfile::tempdir().expect("tempdir");
        let mut base = run(
            NetworkName::Testnet,
            Conclusion::Success,
            QualificationClaim::Partial,
        );
        base.run_id = "run-8".to_string();
        base.scenario_results = vec![result_for(
            "det-status-identity",
            Sdk::Rust,
            Outcome::Passed,
        )];
        emit::write(&base, directory.path()).expect("writes");

        let merged = merge_directory(directory.path(), None, Some("run-8")).expect("merges");
        let outcome = merged.networks.values().next().expect("one network");
        assert!(outcome.runs[0].consumer_findings.is_empty());
    }

    fn result_for(scenario_id: &str, sdk: Sdk, outcome: Outcome) -> ScenarioResult {
        ScenarioResult {
            scenario_id: scenario_id.to_string(),
            sdk,
            runtime: Runtime::ServerSide,
            outcome,
            reason: None,
            classification: None,
            embedded_retry: false,
            duration: Budget::from_seconds(1),
        }
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
