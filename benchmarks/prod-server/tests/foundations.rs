use guardian_prod_benchmarks::cleanup_manifest::{
    CleanupAccountRecord, CleanupAwsTarget, CleanupManifest, CleanupTarget,
};
use guardian_prod_benchmarks::config::RunConfig;
use guardian_prod_benchmarks::report::{
    ArtifactReport, BenchmarkRunReport, CapacityEstimate, CleanupReport, LatencyReport,
    OperationReport, SchemeDistributionReport,
};
use tempfile::tempdir;

#[test]
fn loads_and_validates_profile() {
    let path = "/Users/marcos/repos/guardian/benchmarks/prod-server/profiles/falcon-mixed-burst-scale.toml";
    let config = RunConfig::load_from_path(path.as_ref()).expect("profile should load");

    assert_eq!(config.profile_name, "falcon-mixed-burst-scale");
    assert_eq!(config.users, 4096);
    assert_eq!(config.accounts_per_user, 1);
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
        capacity_estimate: CapacityEstimate {
            target_push_tps: 500.0,
            sustained_push_tps: 42.0,
            headroom_percent: 30.0,
            required_instances: 16,
        },
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
