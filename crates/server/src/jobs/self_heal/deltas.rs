//! Verdicts for the rows of the `deltas` table once the true state is
//! proven: the rows of the proven path become canonical, and every other
//! row is kept only when the true signer set approved it on a true base.

use std::collections::{BTreeSet, HashMap};

use crate::delta_object::{DeltaObject, DeltaStatus, RetainReason};
use crate::network::{AppliedState, NetworkClient};
use crate::services::multisig_admission::TailAccount;
use crate::storage::HealDeltaWrite;

use super::replay::Step;
use super::rows::{RowSource, commitment_key, same_commitment};
use super::types::{AccountInputs, RowAction, RowKind, RowVerdict};

/// The states a kept row or proposal may start from: the true state and
/// the post-state of each kept queued row that chains from it.
pub(super) struct Frontier {
    states: HashMap<String, AppliedState>,
}

impl Frontier {
    fn new(true_state: &AppliedState) -> Self {
        let mut frontier = Self {
            states: HashMap::new(),
        };
        frontier.add(true_state.clone());
        frontier
    }

    fn add(&mut self, state: AppliedState) {
        self.states.insert(commitment_key(&state.commitment), state);
    }

    pub fn get(&self, commitment: &str) -> Option<&AppliedState> {
        self.states.get(&commitment_key(commitment))
    }
}

/// The delta writes and log entries of one account.
pub(super) struct DeltaVerdicts {
    pub writes: Vec<HealDeltaWrite>,
    pub actions: Vec<RowAction>,
    pub has_pending_candidate: bool,
    pub frontier: Frontier,
}

impl DeltaVerdicts {
    fn record(&mut self, write: Option<HealDeltaWrite>, action: RowAction) {
        self.writes.extend(write);
        self.actions.push(action);
    }
}

/// Judges every delta row of one account against its proven path.
pub(super) struct DeltaJudge<'a, 'c> {
    pub client: &'c dyn NetworkClient,
    pub inputs: &'a AccountInputs,
    pub path: &'c [Step<'a>],
}

impl<'a> DeltaJudge<'a, '_> {
    pub fn judge(&self, true_state: &AppliedState) -> DeltaVerdicts {
        let mut verdicts = DeltaVerdicts {
            writes: Vec::new(),
            actions: Vec::new(),
            has_pending_candidate: false,
            frontier: Frontier::new(true_state),
        };
        self.judge_path(&mut verdicts);
        let queued = self.judge_settled_rows(&mut verdicts);
        self.judge_queued(queued, &mut verdicts);
        verdicts
    }

    fn judge_path(&self, verdicts: &mut DeltaVerdicts) {
        for step in self.path {
            let (write, reason) = path_write(step, &self.inputs.now);
            verdicts.record(
                write,
                delta_action(step.row.delta, RowVerdict::Canonical, reason),
            );
        }
    }

    /// Decide every off-path row whose verdict does not depend on another
    /// row, and return the queued rows (candidate or retained) left.
    fn judge_settled_rows(&self, verdicts: &mut DeltaVerdicts) -> Vec<&'a DeltaObject> {
        let path_nonces: BTreeSet<u64> =
            self.path.iter().map(|step| step.row.delta.nonce).collect();
        let mut queued = Vec::new();
        for (index, delta) in self.inputs.deltas.iter().enumerate() {
            if self.on_path(RowSource::Deltas(index)) {
                continue;
            }
            if path_nonces.contains(&delta.nonce) {
                verdicts.actions.push(delta_action(
                    delta,
                    RowVerdict::Delete,
                    "replaced by the proven row at this nonce",
                ));
                continue;
            }
            match &delta.status {
                DeltaStatus::Discarded { .. } => verdicts.actions.push(delta_action(
                    delta,
                    RowVerdict::Keep,
                    "discarded history is kept",
                )),
                DeltaStatus::Canonical { .. } => verdicts.record(
                    Some(HealDeltaWrite::Delete { nonce: delta.nonce }),
                    delta_action(
                        delta,
                        RowVerdict::Delete,
                        "canonical row off the proven path",
                    ),
                ),
                DeltaStatus::Pending { .. } => verdicts.actions.push(delta_action(
                    delta,
                    RowVerdict::Keep,
                    "a pending row is not part of the delta history",
                )),
                DeltaStatus::Candidate { .. } | DeltaStatus::Retained { .. } => queued.push(delta),
            }
        }
        queued.sort_by_key(|delta| delta.nonce);
        queued
    }

    /// Decide the queued rows that chain from the frontier, until no row
    /// links to it. A queued row that never links is deleted.
    fn judge_queued(&self, mut queued: Vec<&'a DeltaObject>, verdicts: &mut DeltaVerdicts) {
        while let Some(position) = queued
            .iter()
            .position(|delta| verdicts.frontier.get(&delta.prev_commitment).is_some())
        {
            let delta = queued.remove(position);
            let Some(base) = verdicts.frontier.get(&delta.prev_commitment).cloned() else {
                continue;
            };
            self.judge_linked(delta, &base, verdicts);
        }
        for delta in queued {
            verdicts.record(
                Some(HealDeltaWrite::Delete { nonce: delta.nonce }),
                delta_action(
                    delta,
                    RowVerdict::Delete,
                    "its base is neither the proven state nor a kept candidate",
                ),
            );
        }
    }

    fn judge_linked(&self, delta: &DeltaObject, base: &AppliedState, verdicts: &mut DeltaVerdicts) {
        match self.authorized_post_state(delta, base) {
            Some(post_state) => self.keep_queued(delta, post_state, verdicts),
            None => self.park_queued(delta, verdicts),
        }
    }

    /// The post-state of `delta` on `base`, when the true signer set
    /// approved it under the push gate.
    fn authorized_post_state(
        &self,
        delta: &DeltaObject,
        base: &AppliedState,
    ) -> Option<AppliedState> {
        let TailAccount::Multisig(multisig) = TailAccount::decode(&base.state_json) else {
            return None;
        };
        multisig
            .authorize(&delta.delta_payload, self.proposal_for(delta))
            .ok()?;
        self.client
            .apply_delta(&base.state_json, &delta.delta_payload)
            .ok()
    }

    fn proposal_for(&self, delta: &DeltaObject) -> Option<&'a DeltaObject> {
        let id = self
            .client
            .delta_proposal_id(&delta.account_id, delta.nonce, &delta.delta_payload)
            .ok()?;
        self.inputs
            .proposals
            .iter()
            .find(|record| same_commitment(&record.commitment, &id))
            .map(|record| &record.proposal)
    }

    fn keep_queued(
        &self,
        delta: &DeltaObject,
        post_state: AppliedState,
        verdicts: &mut DeltaVerdicts,
    ) {
        let stored_matches = delta
            .new_commitment
            .as_deref()
            .is_some_and(|stored| same_commitment(stored, &post_state.commitment));
        let write = (!stored_matches).then(|| {
            HealDeltaWrite::Put(DeltaObject {
                new_commitment: Some(post_state.commitment.clone()),
                ..delta.clone()
            })
        });
        let reason = if stored_matches {
            "approved by the true signer set on a true base"
        } else {
            "approved by the true signer set on a true base; new commitment recomputed"
        };
        if delta.status.is_candidate() {
            verdicts.has_pending_candidate = true;
        }
        verdicts.frontier.add(post_state);
        verdicts.record(write, delta_action(delta, RowVerdict::Keep, reason));
    }

    fn park_queued(&self, delta: &DeltaObject, verdicts: &mut DeltaVerdicts) {
        match &delta.status {
            DeltaStatus::Candidate { .. } => verdicts.record(
                Some(HealDeltaWrite::Put(DeltaObject {
                    status: DeltaStatus::retained(self.inputs.now.clone(), RetainReason::Orphaned),
                    ..delta.clone()
                })),
                delta_action(
                    delta,
                    RowVerdict::Retain,
                    "no proposal of the true signer set approves it",
                ),
            ),
            DeltaStatus::Retained { .. }
            | DeltaStatus::Canonical { .. }
            | DeltaStatus::Discarded { .. }
            | DeltaStatus::Pending { .. } => verdicts.actions.push(delta_action(
                delta,
                RowVerdict::Keep,
                "no proposal of the true signer set approves it; it stays parked",
            )),
        }
    }

    fn on_path(&self, source: RowSource) -> bool {
        self.path.iter().any(|step| step.row.source == source)
    }
}

/// The write that makes a path row canonical with its recomputed
/// commitment, or `None` when the row already is.
fn path_write(step: &Step<'_>, now: &str) -> (Option<HealDeltaWrite>, &'static str) {
    let row = step.row;
    let recomputed = &step.applied.commitment;
    let stored_matches = row
        .delta
        .new_commitment
        .as_deref()
        .is_some_and(|stored| same_commitment(stored, recomputed));
    let timestamp = match (row.source, &row.delta.status) {
        (RowSource::Deltas(_), DeltaStatus::Canonical { .. }) if stored_matches => {
            return (None, "already canonical on the proven path");
        }
        (RowSource::Deltas(_), DeltaStatus::Canonical { timestamp }) => timestamp.clone(),
        (RowSource::Deltas(_), _) | (RowSource::Superseded(_), _) => now.to_string(),
    };
    let put = HealDeltaWrite::Put(DeltaObject {
        new_commitment: Some(recomputed.clone()),
        status: DeltaStatus::canonical(timestamp),
        ..row.delta.clone()
    });
    (Some(put), "on the proven path")
}

fn delta_action(delta: &DeltaObject, verdict: RowVerdict, reason: &str) -> RowAction {
    RowAction {
        row: RowKind::Delta,
        id: delta.nonce.to_string(),
        verdict,
        reason: reason.to_string(),
    }
}
