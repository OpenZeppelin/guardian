use guardian_prod_benchmarks::config::SchemeDistribution;
use guardian_prod_benchmarks::model::AuthScheme;
use guardian_prod_benchmarks::operations::OperationKind;
use guardian_prod_benchmarks::schemes::build_scheme_plan;
use guardian_prod_benchmarks::workload::{operation_for_index, warmup_operation};

#[test]
fn operation_cycle_should_match_four_reads_per_push() {
    let expected = [
        OperationKind::GetState,
        OperationKind::GetState,
        OperationKind::GetState,
        OperationKind::GetState,
        OperationKind::PushDelta,
    ];

    for (index, operation) in expected.into_iter().enumerate() {
        assert_eq!(operation_for_index(4, index as u64), operation);
    }
}

#[test]
fn operation_cycle_should_support_push_only_runs() {
    for index in 0..8 {
        assert_eq!(operation_for_index(0, index), OperationKind::PushDelta);
    }
}

#[test]
fn scheme_plan_should_respect_distribution() {
    let plan = build_scheme_plan(
        4,
        &SchemeDistribution {
            falcon_percent: 50,
            ecdsa_percent: 50,
        },
    );

    assert_eq!(plan.len(), 4);
    assert_eq!(plan[0], AuthScheme::Falcon);
    assert_eq!(plan[1], AuthScheme::Falcon);
    assert_eq!(plan[2], AuthScheme::Ecdsa);
    assert_eq!(plan[3], AuthScheme::Ecdsa);
}

#[test]
fn warmup_should_stay_read_only() {
    assert_eq!(warmup_operation(), OperationKind::GetState);
}
