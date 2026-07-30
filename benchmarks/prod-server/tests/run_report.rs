use guardian_prod_benchmarks::cleanup_manifest::CleanupStatus;
use guardian_prod_benchmarks::model::{
    AuthScheme, CanonicalizationOutcome, CanonicalizationSample,
};
use guardian_prod_benchmarks::report::{
    ArtifactReport, BenchmarkRunReport, CanonicalizationReport, CapacityEstimate, CleanupReport,
    LatencyReport, OperationReport, SchemeDistributionReport,
};

#[test]
fn report_should_include_all_and_scheme_scopes() {
    let report = BenchmarkRunReport {
        run_id: "run-123".to_string(),
        profile_name: "falcon-ecdsa-mixed-burst-scale".to_string(),
        started_at: chrono::Utc::now(),
        completed_at: chrono::Utc::now(),
        measurement_seconds: 10.0,
        guardian_endpoint: "https://guardian.openzeppelin.com:443".to_string(),
        deployment_shape: Some("prod-single-task-arm64-rds-proxy".to_string()),
        scheme_distribution: SchemeDistributionReport {
            falcon_percent: 50,
            ecdsa_percent: 50,
        },
        operations: vec![
            sample_operation("get_state", "falcon"),
            sample_operation("get_state", "ecdsa"),
            sample_operation("get_state", "all"),
            sample_operation("push_delta", "falcon"),
            sample_operation("push_delta", "ecdsa"),
            sample_operation("push_delta", "all"),
        ],
        canonicalization: None,
        pacing: None,
        capacity_estimate: Some(CapacityEstimate {
            target_push_tps: 500.0,
            sustained_push_tps: 42.0,
            headroom_percent: 30.0,
            required_instances: 17,
        }),
        cleanup: CleanupReport {
            manifest_path: "cleanup-manifest.json".to_string(),
            status: CleanupStatus::Pending,
        },
        artifacts: ArtifactReport {
            summary_markdown: "summary.md".to_string(),
            report_json: "run-report.json".to_string(),
            canonicalization_samples: None,
        },
    };

    assert_eq!(report.operations.len(), 6);
    assert!(
        report
            .operations
            .iter()
            .any(|operation| operation.operation == "push_delta" && operation.scope == "all")
    );
}

#[test]
fn canonicalization_report_should_use_canonical_waits_only() {
    let samples = vec![
        canonicalization_sample(1, CanonicalizationOutcome::Canonical, 10_000.0),
        canonicalization_sample(2, CanonicalizationOutcome::Canonical, 20_000.0),
        canonicalization_sample(3, CanonicalizationOutcome::Canonical, 30_000.0),
        canonicalization_sample(4, CanonicalizationOutcome::Discarded, 5_000.0),
        canonicalization_sample(5, CanonicalizationOutcome::TimedOut, 180_000.0),
        canonicalization_sample(6, CanonicalizationOutcome::ObservationFailed, 1_000.0),
    ];

    let report = CanonicalizationReport::from_samples(&samples, 180)
        .expect("samples should produce a report");
    assert_eq!(report.sampled, 6);
    assert_eq!(report.canonical, 3);
    assert_eq!(report.discarded, 1);
    assert_eq!(report.timed_out, 1);
    assert_eq!(report.observation_failed, 1);
    assert_eq!(report.timeout_seconds, 180);
    assert_eq!(report.wait_ms.p50, 20_000.0);
    assert_eq!(report.wait_ms.max, 30_000.0);
}

#[test]
fn canonicalization_report_should_be_absent_without_samples() {
    assert!(CanonicalizationReport::from_samples(&[], 180).is_none());
}

fn canonicalization_sample(
    nonce: u64,
    outcome: CanonicalizationOutcome,
    wait_ms: f64,
) -> CanonicalizationSample {
    CanonicalizationSample {
        account_id: "0x1".to_string(),
        auth_scheme: AuthScheme::Ecdsa,
        nonce,
        outcome,
        wait_ms,
        polls: 1,
        observation_error: (outcome == CanonicalizationOutcome::ObservationFailed)
            .then(|| "transport error".to_string()),
    }
}

fn sample_operation(operation: &str, scope: &str) -> OperationReport {
    OperationReport {
        operation: operation.to_string(),
        scope: scope.to_string(),
        attempted: 10,
        succeeded: 9,
        failed: 1,
        throughput_ops_per_sec: 12.0,
        latency_ms: LatencyReport {
            p50: 10.0,
            p95: 12.0,
            p99: 14.0,
            max: 15.0,
        },
        failure_breakdown: Default::default(),
    }
}
