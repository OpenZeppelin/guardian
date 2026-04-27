use crate::artifacts::prepare_artifacts;
use crate::cleanup;
use crate::cleanup_manifest::{CleanupAwsTarget, CleanupManifest, CleanupStatus, CleanupTarget};
use crate::config::RunConfig;
use crate::distributed::{
    ExecutionShard, WorkerArtifact, load_worker_artifacts, merge_worker_operations,
};
use crate::report::{
    ArtifactReport, BenchmarkRunReport, CapacityEstimate, CleanupReport, SchemeDistributionReport,
};
use crate::runner::RunOutput;
use crate::seed::seed_users;
use anyhow::{Error, Result, anyhow};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::{Path, PathBuf};

struct ExecutedBenchmark {
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    run_output: RunOutput,
}

struct RunReportInput<'a> {
    config: &'a RunConfig,
    run_id: String,
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    measurement_seconds: f64,
    operations: Vec<crate::report::OperationReport>,
    cleanup_manifest: &'a CleanupManifest,
    artifacts: &'a crate::artifacts::ArtifactPaths,
}

pub async fn run_worker(
    profile: &Path,
    run_id: String,
    shard_index: u32,
    shard_count: u32,
) -> Result<()> {
    let config = RunConfig::load_from_path(profile)?;
    let shard = ExecutionShard::new(shard_index, shard_count)?;
    let execution = execute_profile(&config, shard).await?;
    let worker_artifact = WorkerArtifact {
        run_id,
        worker_id: WorkerArtifact::worker_id(shard),
        shard_index,
        shard_count,
        profile_name: config.profile_name.clone(),
        guardian_endpoint: config.normalized_guardian_endpoint(),
        deployment_shape: config.deployment_shape.clone(),
        started_at: execution.started_at,
        completed_at: execution.completed_at,
        measurement_seconds: execution.run_output.measurement_seconds,
        operations: execution.run_output.operations,
        cleanup_accounts: execution.run_output.cleanup_accounts,
    };

    println!("{}", worker_artifact.encoded_line()?);
    for operation in &worker_artifact.operations {
        println!(
            "worker={} operation={} scope={} success={} throughput={:.2}",
            worker_artifact.worker_id,
            operation.operation,
            operation.scope,
            operation.succeeded,
            operation.throughput_ops_per_sec
        );
    }

    Ok(())
}

pub async fn aggregate(
    profile: &Path,
    run_id: &str,
    worker_artifact_paths: &[PathBuf],
    no_cleanup: bool,
) -> Result<()> {
    let config = RunConfig::load_from_path(profile)?;
    let worker_artifacts = load_worker_artifacts(worker_artifact_paths)?;
    let artifacts = prepare_artifacts(&config.artifacts_dir, run_id)?;
    persist_worker_artifacts(&artifacts, worker_artifacts.as_slice())?;

    let started_at = worker_artifacts
        .iter()
        .map(|artifact| artifact.started_at)
        .min()
        .ok_or_else(|| anyhow!("at least one worker artifact is required"))?;
    let completed_at = worker_artifacts
        .iter()
        .map(|artifact| artifact.completed_at)
        .max()
        .ok_or_else(|| anyhow!("at least one worker artifact is required"))?;

    let measurement_seconds = worker_artifacts
        .iter()
        .map(|artifact| artifact.measurement_seconds)
        .fold(0.0_f64, f64::max)
        .max(0.001);
    let operations = merge_worker_operations(worker_artifacts.as_slice(), measurement_seconds)?;
    let cleanup_target = build_cleanup_target(&config);
    let mut manifest = CleanupManifest::new(
        run_id.to_string(),
        config.normalized_guardian_endpoint(),
        cleanup_target,
    );
    manifest.accounts = worker_artifacts
        .iter()
        .flat_map(|artifact| artifact.cleanup_accounts.clone())
        .collect();
    manifest.purge_status = CleanupStatus::Pending;
    manifest.write_to_path(&artifacts.cleanup_manifest)?;

    let (cleanup_manifest, cleanup_error) =
        maybe_cleanup(&config, &artifacts.cleanup_manifest, no_cleanup).await?;
    let report = build_run_report(RunReportInput {
        config: &config,
        run_id: run_id.to_string(),
        started_at,
        completed_at,
        measurement_seconds,
        operations,
        cleanup_manifest: &cleanup_manifest,
        artifacts: &artifacts,
    });

    persist_report(&artifacts, &report)?;
    print_report_summary(&report, &artifacts);
    if let Some(error) = cleanup_error {
        return Err(error);
    }
    Ok(())
}

async fn execute_profile(config: &RunConfig, shard: ExecutionShard) -> Result<ExecutedBenchmark> {
    let started_at = Utc::now();
    let users = seed_users(config, shard).await?;
    let run_output = crate::runner::execute(config, users).await?;
    let completed_at = Utc::now();

    Ok(ExecutedBenchmark {
        started_at,
        completed_at,
        run_output,
    })
}

async fn maybe_cleanup(
    config: &RunConfig,
    manifest_path: &Path,
    no_cleanup: bool,
) -> Result<(CleanupManifest, Option<Error>)> {
    if no_cleanup || !config.cleanup.enabled {
        return Ok((CleanupManifest::load_from_path(manifest_path)?, None));
    }

    match cleanup::purge_manifest_path(manifest_path).await {
        Ok(cleaned) => Ok((cleaned, None)),
        Err(error) => {
            let manifest = CleanupManifest::load_from_path(manifest_path)?;
            Ok((manifest, Some(error)))
        }
    }
}

fn build_run_report(input: RunReportInput<'_>) -> BenchmarkRunReport {
    let RunReportInput {
        config,
        run_id,
        started_at,
        completed_at,
        measurement_seconds,
        operations,
        cleanup_manifest,
        artifacts,
    } = input;
    let push_throughput = operations
        .iter()
        .find(|operation| operation.operation == "push_delta" && operation.scope == "all")
        .map(|operation| operation.throughput_ops_per_sec)
        .unwrap_or(0.0);
    let headroom_percent = 30.0;
    let effective_per_instance = (push_throughput * (1.0 - headroom_percent / 100.0)).max(0.001);
    let required_instances = (500.0 / effective_per_instance).ceil() as u32;

    BenchmarkRunReport {
        run_id,
        profile_name: config.profile_name.clone(),
        started_at,
        completed_at,
        measurement_seconds,
        guardian_endpoint: config.normalized_guardian_endpoint(),
        deployment_shape: config.deployment_shape.clone(),
        scheme_distribution: SchemeDistributionReport {
            falcon_percent: config.scheme_distribution.falcon_percent,
            ecdsa_percent: config.scheme_distribution.ecdsa_percent,
        },
        operations,
        capacity_estimate: CapacityEstimate {
            target_push_tps: 500.0,
            sustained_push_tps: push_throughput,
            headroom_percent,
            required_instances: required_instances.max(1),
        },
        cleanup: CleanupReport {
            manifest_path: artifacts.cleanup_manifest.display().to_string(),
            status: cleanup_manifest.purge_status.clone(),
        },
        artifacts: ArtifactReport {
            summary_markdown: artifacts.summary_markdown.display().to_string(),
            report_json: artifacts.report_json.display().to_string(),
            canonicalization_samples: None,
        },
    }
}

fn persist_report(
    artifacts: &crate::artifacts::ArtifactPaths,
    report: &BenchmarkRunReport,
) -> Result<()> {
    report.write_to_path(&artifacts.report_json)?;
    fs::write(&artifacts.summary_markdown, render_summary(report))?;
    Ok(())
}

fn persist_worker_artifacts(
    artifacts: &crate::artifacts::ArtifactPaths,
    worker_artifacts: &[WorkerArtifact],
) -> Result<()> {
    let workers_dir = artifacts.aws_dir.join("workers");
    fs::create_dir_all(&workers_dir)?;
    for artifact in worker_artifacts {
        let path = workers_dir.join(format!("{}.json", artifact.worker_id));
        artifact.write_to_path(&path)?;
    }
    Ok(())
}

fn print_report_summary(report: &BenchmarkRunReport, artifacts: &crate::artifacts::ArtifactPaths) {
    println!("run_id={}", report.run_id);
    println!("report={}", artifacts.report_json.display());
    println!("summary={}", artifacts.summary_markdown.display());
    println!("manifest={}", artifacts.cleanup_manifest.display());
    println!("cleanup_status={:?}", report.cleanup.status);
    for operation in &report.operations {
        println!(
            "operation={} scope={} success={} throughput={:.2}",
            operation.operation,
            operation.scope,
            operation.succeeded,
            operation.throughput_ops_per_sec
        );
    }
}

fn build_cleanup_target(config: &RunConfig) -> CleanupTarget {
    let ecs_container = config
        .aws
        .ecs_container
        .clone()
        .unwrap_or_else(|| config.aws.ecs_service.clone());
    CleanupTarget {
        aws: CleanupAwsTarget {
            profile: config.aws.profile.clone(),
            region: config.aws.region.clone(),
            ecs_cluster: config.aws.ecs_cluster.clone(),
            ecs_service: config.aws.ecs_service.clone(),
            ecs_container,
        },
    }
}

fn render_summary(report: &BenchmarkRunReport) -> String {
    let mut output = String::new();
    output.push_str("# Benchmark Summary\n\n");
    output.push_str(&format!("Run ID: `{}`\n", report.run_id));
    output.push_str(&format!("Profile: `{}`\n", report.profile_name));
    output.push_str(&format!("Endpoint: `{}`\n", report.guardian_endpoint));
    output.push_str(&format!(
        "Measured duration: `{:.2}s`\n",
        report.measurement_seconds
    ));
    if let Some(shape) = &report.deployment_shape {
        output.push_str(&format!("Deployment shape: `{shape}`\n"));
    }
    output.push('\n');
    output.push_str("| Operation | Scope | Attempted | Succeeded | Failed | Throughput |\n");
    output.push_str("|-----------|-------|-----------|-----------|--------|------------|\n");
    for operation in &report.operations {
        output.push_str(&format!(
            "| {} | {} | {} | {} | {} | {:.2} ops/s |\n",
            operation.operation,
            operation.scope,
            operation.attempted,
            operation.succeeded,
            operation.failed,
            operation.throughput_ops_per_sec
        ));
    }
    output.push('\n');
    output.push_str(&format!(
        "Estimated instances for 500 TPS with {:.0}% headroom: `{}`\n",
        report.capacity_estimate.headroom_percent, report.capacity_estimate.required_instances
    ));
    output.push_str(&format!("Cleanup status: `{:?}`\n", report.cleanup.status));
    output
}
