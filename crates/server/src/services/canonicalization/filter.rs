use crate::canonicalization::CanonicalizationConfig;
use crate::storage::DeltaObject;
use chrono::{DateTime, Utc};

pub fn filter_pending_candidates(deltas: &[DeltaObject]) -> Vec<DeltaObject> {
    let mut candidates: Vec<DeltaObject> = deltas
        .iter()
        .filter(|delta| delta.status.is_candidate())
        .cloned()
        .collect();

    candidates.sort_by_key(|d| d.nonce);
    candidates
}

pub fn filter_ready_candidates(
    deltas: &[DeltaObject],
    config: &CanonicalizationConfig,
) -> Vec<DeltaObject> {
    let now = Utc::now();
    let mut candidates: Vec<DeltaObject> = deltas
        .iter()
        .filter(|delta| is_ready_candidate(delta, &now, config))
        .cloned()
        .collect();

    candidates.sort_by_key(|d| d.nonce);
    candidates
}

fn is_ready_candidate(
    delta: &DeltaObject,
    now: &DateTime<Utc>,
    config: &CanonicalizationConfig,
) -> bool {
    if !delta.status.is_candidate() {
        return false;
    }

    let candidate_at_str = delta.status.timestamp();
    if let Ok(candidate_at) = DateTime::parse_from_rfc3339(candidate_at_str) {
        let elapsed = now.signed_duration_since(candidate_at);
        return elapsed.num_seconds() >= config.delay_seconds as i64;
    }

    false
}
