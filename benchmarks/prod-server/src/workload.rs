use crate::config::{ReadMode, RoleConfig};
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

pub fn canonicalization_sample_decision(sample_rate: f64, roll: f64) -> bool {
    sample_rate > 0.0 && roll < sample_rate
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Writer,
    Reader,
    Idle,
}

pub fn role_for_user(roles: &RoleConfig, user_index: u32) -> Role {
    if user_index < roles.writers {
        Role::Writer
    } else if user_index - roles.writers < roles.readers {
        Role::Reader
    } else {
        Role::Idle
    }
}

/// The writer whose account the reader of rank `reader_rank` (its position
/// among the readers seeded alongside `writers`) polls as a cosigner, or
/// `None` when it polls an account of its own.
pub fn watched_writer(roles: &RoleConfig, writers: &[u32], reader_rank: usize) -> Option<u32> {
    let watching = writers.len() * roles.cosigners_per_writer as usize;
    (reader_rank < watching).then(|| writers[reader_rank % writers.len()])
}

pub fn reader_fetches_state(
    mode: ReadMode,
    previous_nonce: Option<u64>,
    observed_nonce: u64,
) -> bool {
    match mode {
        ReadMode::FullState => true,
        ReadMode::NonceThenStateOnChange => previous_nonce != Some(observed_nonce),
    }
}
