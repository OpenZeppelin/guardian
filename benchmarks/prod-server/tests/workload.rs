use guardian_prod_benchmarks::config::{LoadModel, OperationMix, SchemeDistribution};
use guardian_prod_benchmarks::model::AuthScheme;
use guardian_prod_benchmarks::operations::OperationKind;
use guardian_prod_benchmarks::report::PacingReport;
use guardian_prod_benchmarks::schemes::build_scheme_plan;
use guardian_prod_benchmarks::workload::{
    canonicalization_sample_decision, operation_for_index, warmup_operation,
};
use std::time::Duration;

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

#[test]
fn paced_model_exposes_its_interval_and_closed_loop_does_not() {
    let paced = LoadModel::Paced {
        read_interval_ms: 100,
    };
    assert_eq!(paced.read_interval_ms(), Some(100));
    assert_eq!(paced.interval(), Some(Duration::from_millis(100)));
    assert_eq!(LoadModel::ClosedLoop.read_interval_ms(), None);
    assert_eq!(LoadModel::ClosedLoop.interval(), None);
}

// Offered rate is users / interval, and it is the number a paced run is
// judged on. Getting it wrong silently changes what the target means.
#[test]
fn declared_rate_is_users_over_interval() {
    let report = PacingReport::new(100, 64, 0, 0, 1.0);
    assert_eq!(report.declared_rate_per_sec, 640.0);

    // The target's own shape: 20,000 readers at one read per 10s.
    let target = PacingReport::new(10_000, 20_000, 0, 0, 1.0);
    assert_eq!(target.declared_rate_per_sec, 2_000.0);
}

#[test]
fn a_run_that_slips_past_tolerance_does_not_hold_its_rate() {
    let held = PacingReport::new(100, 64, 10_000, 100, 100.0);
    assert_eq!(held.slipped_percent, 1.0);
    assert!(held.held_declared_rate);

    let missed = PacingReport::new(100, 64, 10_000, 101, 100.0);
    assert!(
        !missed.held_declared_rate,
        "1.01% slip should not count as held"
    );
}

#[test]
fn offered_rate_reports_what_the_generator_actually_produced() {
    // Half the ticks the declared rate implies over the window.
    let report = PacingReport::new(100, 64, 32_000, 0, 100.0);
    assert_eq!(report.declared_rate_per_sec, 640.0);
    assert_eq!(report.offered_rate_per_sec, 320.0);
}
