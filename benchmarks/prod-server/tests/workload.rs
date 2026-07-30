use guardian_prod_benchmarks::config::{OperationMix, SchemeDistribution};
use guardian_prod_benchmarks::model::AuthScheme;
use guardian_prod_benchmarks::operations::OperationKind;
use guardian_prod_benchmarks::schemes::build_scheme_plan;
use guardian_prod_benchmarks::workload::{
    canonicalization_sample_decision, operation_for_index, warmup_operation,
};

#[test]
fn operation_cycle_should_match_four_reads_per_push() {
    let mix = OperationMix::Mixed {
        reads_per_push: 4,
        retire_after_first_successful_push: false,
    };
    let expected = [
        OperationKind::GetState,
        OperationKind::GetState,
        OperationKind::GetState,
        OperationKind::GetState,
        OperationKind::PushDelta,
    ];

    for (index, operation) in expected.into_iter().enumerate() {
        assert_eq!(operation_for_index(&mix, index as u64), operation);
    }
}

#[test]
fn operation_cycle_should_support_push_only_runs() {
    let mix = OperationMix::PushOnly {
        retire_after_first_successful_push: false,
    };
    for index in 0..8 {
        assert_eq!(operation_for_index(&mix, index), OperationKind::PushDelta);
    }
}

#[test]
fn read_only_runs_should_never_push() {
    for index in 0..64 {
        assert_eq!(
            operation_for_index(&OperationMix::ReadOnly, index),
            OperationKind::GetState
        );
    }
}

#[test]
fn read_only_runs_should_never_retire_accounts() {
    assert!(!OperationMix::ReadOnly.retires_after_first_successful_push());
    assert!(!OperationMix::ReadOnly.pushes());
    assert!(
        OperationMix::PushOnly {
            retire_after_first_successful_push: true
        }
        .retires_after_first_successful_push()
    );
    assert!(
        OperationMix::Mixed {
            reads_per_push: 4,
            retire_after_first_successful_push: true
        }
        .retires_after_first_successful_push()
    );
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

#[test]
fn canonicalization_sampling_should_respect_rate_bounds() {
    assert!(!canonicalization_sample_decision(0.0, 0.0));
    assert!(!canonicalization_sample_decision(0.0, 0.5));
    assert!(canonicalization_sample_decision(0.05, 0.04));
    assert!(!canonicalization_sample_decision(0.05, 0.05));
    assert!(canonicalization_sample_decision(1.0, 0.999));
}
