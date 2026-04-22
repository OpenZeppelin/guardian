use chrono::Utc;
use guardian_prod_benchmarks::cleanup_manifest::CleanupAccountRecord;
use guardian_prod_benchmarks::distributed::{
    ExecutionShard, WorkerArtifact, merge_worker_operations,
};
use guardian_prod_benchmarks::report::{LatencyReport, OperationReport};

#[test]
fn shard_assignment_should_round_robin_users() {
    let shard = ExecutionShard::new(1, 3).expect("valid shard");
    assert_eq!(shard.assigned_user_ids(8), vec![1, 4, 7]);
}

#[test]
fn merge_worker_operations_should_sum_counts() {
    let workers = vec![
        WorkerArtifact {
            run_id: "run-1".to_string(),
            worker_id: "shard-0-of-2".to_string(),
            shard_index: 0,
            shard_count: 2,
            profile_name: "mixed".to_string(),
            guardian_endpoint: "https://guardian.openzeppelin.com:443".to_string(),
            deployment_shape: Some("prod".to_string()),
            started_at: Utc::now(),
            completed_at: Utc::now(),
            measurement_seconds: 4.0,
            operations: vec![sample_operation("push_delta", "all", 10, 8, 2, 4.0, 100.0)],
            cleanup_accounts: vec![sample_account("0x1")],
        },
        WorkerArtifact {
            run_id: "run-1".to_string(),
            worker_id: "shard-1-of-2".to_string(),
            shard_index: 1,
            shard_count: 2,
            profile_name: "mixed".to_string(),
            guardian_endpoint: "https://guardian.openzeppelin.com:443".to_string(),
            deployment_shape: Some("prod".to_string()),
            started_at: Utc::now(),
            completed_at: Utc::now(),
            measurement_seconds: 5.0,
            operations: vec![sample_operation("push_delta", "all", 12, 10, 2, 5.0, 200.0)],
            cleanup_accounts: vec![sample_account("0x2")],
        },
    ];

    let merged = merge_worker_operations(&workers, 5.0).expect("merge should succeed");
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].attempted, 22);
    assert_eq!(merged[0].succeeded, 18);
    assert_eq!(merged[0].failed, 4);
    assert_eq!(merged[0].throughput_ops_per_sec, 3.6);
    assert_eq!(merged[0].latency_ms.max, 200.0);
}

fn sample_operation(
    operation: &str,
    scope: &str,
    attempted: u64,
    succeeded: u64,
    failed: u64,
    p50: f64,
    max: f64,
) -> OperationReport {
    OperationReport {
        operation: operation.to_string(),
        scope: scope.to_string(),
        attempted,
        succeeded,
        failed,
        throughput_ops_per_sec: succeeded as f64 / 5.0,
        latency_ms: LatencyReport {
            p50,
            p95: p50 + 1.0,
            p99: p50 + 2.0,
            max,
        },
        failure_breakdown: Default::default(),
    }
}

fn sample_account(account_id: &str) -> CleanupAccountRecord {
    CleanupAccountRecord {
        account_id: account_id.to_string(),
        owner_user_id: 0,
        auth_scheme: "falcon".to_string(),
        created_delta_nonces: vec![],
        last_known_commitment: None,
    }
}
