use guardian_prod_benchmarks::cleanup_manifest::{
    CleanupAccountRecord, CleanupAwsTarget, CleanupManifest, CleanupTarget,
};
use guardian_prod_benchmarks::config::{OperationMix, RunConfig};
use guardian_prod_benchmarks::report::{
    ArtifactReport, BenchmarkRunReport, CapacityEstimate, CleanupReport, LatencyReport,
    OperationReport, SchemeDistributionReport,
};
use std::path::PathBuf;
use tempfile::tempdir;

fn profile_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("profiles")
        .join(name)
}

#[test]
fn loads_and_validates_profile() {
    let path = profile_path("falcon-mixed-burst-scale.toml");
    let config = RunConfig::load_from_path(path.as_path()).expect("profile should load");

    assert_eq!(config.profile_name, "falcon-mixed-burst-scale");
    assert_eq!(config.users, 4096);
    assert_eq!(config.accounts_per_user, 1);
}

// A profile that fails to parse is otherwise only discovered after the
// benchmark image is built, pushed, and launched on Fargate.
#[test]
fn every_committed_profile_loads_and_validates() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("profiles");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("profiles directory should exist") {
        let path = entry.expect("profile entry should be readable").path();
        if path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        RunConfig::load_from_path(path.as_path())
            .unwrap_or_else(|error| panic!("profile {} should load: {error}", path.display()));
        checked += 1;
    }
    assert!(checked > 0, "no profiles found in {}", dir.display());
}

#[test]
fn read_only_profile_declares_a_read_only_workload() {
    let path = profile_path("read-only-ramp.toml");
    let config = RunConfig::load_from_path(path.as_path()).expect("profile should load");

    assert!(matches!(config.operation_mix, OperationMix::ReadOnly));
    assert!(!config.operation_mix.pushes());
    assert_eq!(config.users, 512);
    assert_eq!(config.scheme_distribution.ecdsa_percent, 100);
    assert!(config.cleanup.enabled);
}

#[test]
fn mixed_mode_rejects_a_zero_read_ratio() {
    let path = profile_path("read-only-ramp.toml");
    let raw = std::fs::read_to_string(&path).expect("profile should be readable");
    let dir = tempdir().expect("tempdir");
    let degenerate = dir.path().join("degenerate.toml");
    std::fs::write(
        &degenerate,
        raw.replace(
            "[operation_mix]\nmode = \"read_only\"",
            "[operation_mix]\nmode = \"mixed\"\nreads_per_push = 0\nretire_after_first_successful_push = false",
        ),
    )
    .expect("degenerate profile should be writable");

    let error = RunConfig::load_from_path(degenerate.as_path())
        .expect_err("mixed mode with no reads should be rejected");
    assert!(
        error.to_string().contains("reads_per_push"),
        "unexpected error: {error}"
    );
}

#[test]
fn cleanup_manifest_roundtrip() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("cleanup-manifest.json");
    let mut manifest = CleanupManifest::new(
        "run-123".to_string(),
        "https://guardian.openzeppelin.com".to_string(),
        CleanupTarget {
            aws: CleanupAwsTarget {
                profile: Some("dev".to_string()),
                region: "us-east-1".to_string(),
                ecs_cluster: "guardian-prod-cluster".to_string(),
                ecs_service: "guardian-prod-server".to_string(),
                ecs_container: "guardian-prod-server".to_string(),
            },
        },
    );
    manifest.accounts.push(CleanupAccountRecord {
        account_id: "0xabc".to_string(),
        owner_user_id: 1,
        auth_scheme: "falcon".to_string(),
        created_delta_nonces: vec![1, 2],
        last_known_commitment: Some("0xdef".to_string()),
    });

    manifest
        .write_to_path(&path)
        .expect("manifest should write");
    let loaded = CleanupManifest::load_from_path(&path).expect("manifest should load");

    assert_eq!(loaded.run_id, "run-123");
    assert_eq!(loaded.accounts.len(), 1);
    assert_eq!(
        loaded.cleanup_target.aws.ecs_service,
        "guardian-prod-server"
    );
}

#[test]
fn run_report_roundtrip() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("run-report.json");
    let report = BenchmarkRunReport {
        run_id: "run-123".to_string(),
        profile_name: "falcon-mixed-burst-scale".to_string(),
        started_at: chrono::Utc::now(),
        completed_at: chrono::Utc::now(),
        measurement_seconds: 12.5,
        guardian_endpoint: "https://guardian.openzeppelin.com".to_string(),
        deployment_shape: Some("prod-single-task-arm64-rds-proxy".to_string()),
        scheme_distribution: SchemeDistributionReport {
            falcon_percent: 100,
            ecdsa_percent: 0,
        },
        operations: vec![OperationReport {
            operation: "get_state".to_string(),
            scope: "all".to_string(),
            attempted: 10,
            succeeded: 10,
            failed: 0,
            throughput_ops_per_sec: 12.5,
            latency_ms: LatencyReport {
                p50: 10.0,
                p95: 12.0,
                p99: 15.0,
                max: 16.0,
            },
            failure_breakdown: Default::default(),
        }],
        canonicalization: None,
        capacity_estimate: Some(CapacityEstimate {
            target_push_tps: 500.0,
            sustained_push_tps: 42.0,
            headroom_percent: 30.0,
            required_instances: 16,
        }),
        cleanup: CleanupReport {
            manifest_path: "cleanup-manifest.json".to_string(),
            status: guardian_prod_benchmarks::cleanup_manifest::CleanupStatus::Pending,
        },
        artifacts: ArtifactReport {
            summary_markdown: "summary.md".to_string(),
            report_json: "run-report.json".to_string(),
            canonicalization_samples: None,
        },
    };

    report.write_to_path(&path).expect("report should write");
    let raw = std::fs::read_to_string(&path).expect("report should read");
    let loaded: BenchmarkRunReport = serde_json::from_str(&raw).expect("report should parse");

    assert_eq!(loaded.profile_name, "falcon-mixed-burst-scale");
    assert_eq!(loaded.operations.len(), 1);
    assert_eq!(loaded.measurement_seconds, 12.5);
}
