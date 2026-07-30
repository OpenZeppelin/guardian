use crate::config::OperationMix;
use crate::operations::OperationKind;

pub fn operation_for_index(mix: &OperationMix, op_index: u64) -> OperationKind {
    match mix {
        OperationMix::ReadOnly => OperationKind::GetState,
        OperationMix::PushOnly { .. } => OperationKind::PushDelta,
        OperationMix::Mixed {
            reads_per_push: 0, ..
        } => OperationKind::PushDelta,
        OperationMix::Mixed { reads_per_push, .. } => {
            let cycle = u64::from(reads_per_push.saturating_add(1));
            if op_index % cycle == u64::from(*reads_per_push) {
                OperationKind::PushDelta
            } else {
                OperationKind::GetState
            }
        }
    }
}

pub fn warmup_operation() -> OperationKind {
    OperationKind::GetState
}

pub fn canonicalization_sample_decision(sample_rate: f64, roll: f64) -> bool {
    sample_rate > 0.0 && roll < sample_rate
}
