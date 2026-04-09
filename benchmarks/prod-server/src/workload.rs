use crate::operations::OperationKind;

pub fn operation_for_index(reads_per_push: u32, op_index: u64) -> OperationKind {
    if reads_per_push == 0 {
        return OperationKind::PushDelta;
    }
    let cycle = u64::from(reads_per_push.saturating_add(1));
    if cycle > 0 && op_index % cycle == u64::from(reads_per_push) {
        OperationKind::PushDelta
    } else {
        OperationKind::GetState
    }
}

pub fn warmup_operation() -> OperationKind {
    OperationKind::GetState
}
