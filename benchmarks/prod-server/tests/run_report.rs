use guardian_prod_benchmarks::cleanup_manifest::CleanupStatus;
use guardian_prod_benchmarks::report::{
    ArtifactReport, BenchmarkRunReport, CapacityEstimate, CleanupReport, LatencyReport,
    OperationReport, SchemeDistributionReport,
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
        capacity_estimate: CapacityEstimate {
            target_push_tps: 500.0,
            sustained_push_tps: 42.0,
            headroom_percent: 30.0,
            required_instances: 17,
        },
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
