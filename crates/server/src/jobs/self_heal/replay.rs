//! The replay: build the account from a creation delta and apply stored
//! rows until the commitment equals the on-chain commitment.

use std::collections::HashSet;

use crate::network::{AppliedState, NetworkClient};

use super::rows::{Row, Rows, commitment_key, same_commitment};

/// The most delta applications the replay makes for each row. Each row
/// is tried once as a base and, because each state is explored once, is
/// applied at most once as a link. The budget is a guard against a fault
/// in the search, not a limit on honest history.
const APPLIES_PER_ROW: usize = 2;

/// One applied row and the state it produced. The replay drops the
/// `state_json` of a step once no later step can need it.
#[derive(Debug, Clone)]
pub(super) struct Step<'a> {
    pub row: Row<'a>,
    pub applied: AppliedState,
}

/// What the replay found.
#[derive(Debug)]
pub(super) enum ReplayOutcome<'a> {
    /// The path from the creation delta to the on-chain commitment. The
    /// last step holds the true state with its `state_json`.
    Reached(Vec<Step<'a>>),
    /// No row builds a base account.
    NoBase,
    /// The rows end before the on-chain commitment. `deepest` is the
    /// longest path tried, without state blobs.
    Short {
        deepest: Vec<Step<'a>>,
        canonical_link_failed: bool,
        budget_exceeded: bool,
    },
}

/// Run the replay for one account.
pub(super) fn replay<'a>(
    client: &dyn NetworkClient,
    account_id: &str,
    rows: &Rows<'a>,
    target: &str,
) -> ReplayOutcome<'a> {
    Search::new(client, rows, target).run(account_id)
}

/// The account a creation row builds with no prior state, when that
/// account is `account_id`.
pub(super) fn build_base(
    client: &dyn NetworkClient,
    account_id: &str,
    row: &Row<'_>,
) -> Option<AppliedState> {
    let applied = client
        .apply_delta(&serde_json::Value::Null, &row.delta.delta_payload)
        .ok()?;
    let head = client
        .get_state_head(account_id, &applied.state_json)
        .ok()?;
    same_commitment(&head.commitment, &applied.commitment).then_some(applied)
}

struct Frame<'a, 'r> {
    links: &'r [Row<'a>],
    next: usize,
}

impl Frame<'_, '_> {
    fn is_exhausted(&self) -> bool {
        self.next >= self.links.len()
    }
}

enum Walk {
    Reached,
    Dead,
    BudgetExceeded,
}

struct Search<'a, 'r, 'c> {
    client: &'c dyn NetworkClient,
    rows: &'r Rows<'a>,
    target: &'c str,
    explored: HashSet<String>,
    applies: usize,
    budget: usize,
    canonical_link_failed: bool,
    path: Vec<Step<'a>>,
    deepest: Vec<Step<'a>>,
}

impl<'a, 'r, 'c> Search<'a, 'r, 'c> {
    fn new(client: &'c dyn NetworkClient, rows: &'r Rows<'a>, target: &'c str) -> Self {
        Self {
            client,
            rows,
            target,
            explored: HashSet::new(),
            applies: 0,
            budget: rows.all().len() * APPLIES_PER_ROW + 1,
            canonical_link_failed: false,
            path: Vec::new(),
            deepest: Vec::new(),
        }
    }

    fn run(mut self, account_id: &str) -> ReplayOutcome<'a> {
        let mut built_any = false;
        for row in self.rows.all() {
            if self.applies >= self.budget {
                return self.short(true);
            }
            self.applies += 1;
            let Some(base) = build_base(self.client, account_id, row) else {
                continue;
            };
            built_any = true;
            match self.walk_from(Step {
                row: *row,
                applied: base,
            }) {
                Walk::Reached => return ReplayOutcome::Reached(self.path),
                Walk::BudgetExceeded => return self.short(true),
                Walk::Dead => {}
            }
        }
        if built_any {
            self.short(false)
        } else {
            ReplayOutcome::NoBase
        }
    }

    fn short(mut self, budget_exceeded: bool) -> ReplayOutcome<'a> {
        self.keep_if_deepest();
        ReplayOutcome::Short {
            deepest: self.deepest,
            canonical_link_failed: self.canonical_link_failed,
            budget_exceeded,
        }
    }

    fn walk_from(&mut self, base: Step<'a>) -> Walk {
        let mut frames: Vec<Frame<'a, 'r>> = Vec::new();
        if !self.enter(&mut frames, base) {
            return self.reached_or_dead();
        }
        while let Some(frame) = frames.last_mut() {
            if frame.is_exhausted() {
                frames.pop();
                self.leave();
                continue;
            }
            let row = frame.links[frame.next];
            frame.next += 1;
            if self.path_holds_nonce(row.delta.nonce) {
                continue;
            }
            if self.applies >= self.budget {
                return Walk::BudgetExceeded;
            }
            self.applies += 1;
            let Some(applied) = self.apply(&row) else {
                continue;
            };
            if !self.enter(&mut frames, Step { row, applied }) {
                if self.at_target() {
                    return Walk::Reached;
                }
                self.leave();
            }
        }
        Walk::Dead
    }

    /// Push `step` on the path. Returns true when the search continues
    /// from it: it is not the target and was not explored before.
    fn enter(&mut self, frames: &mut Vec<Frame<'a, 'r>>, step: Step<'a>) -> bool {
        let commitment = step.applied.commitment.clone();
        self.release_exhausted_tip(frames);
        self.path.push(step);
        if same_commitment(&commitment, self.target) {
            return false;
        }
        if !self.explored.insert(commitment_key(&commitment)) {
            return false;
        }
        let rows: &'r Rows<'a> = self.rows;
        frames.push(Frame {
            links: rows.based_on(&commitment),
            next: 0,
        });
        true
    }

    fn reached_or_dead(&mut self) -> Walk {
        if self.at_target() {
            Walk::Reached
        } else {
            self.leave();
            Walk::Dead
        }
    }

    fn at_target(&self) -> bool {
        self.path
            .last()
            .is_some_and(|step| same_commitment(&step.applied.commitment, self.target))
    }

    fn leave(&mut self) {
        self.keep_if_deepest();
        self.path.pop();
    }

    /// The tip's blob is needed only to apply its remaining links; once
    /// its frame has none, a later step never reads it again.
    fn release_exhausted_tip(&mut self, frames: &[Frame<'a, 'r>]) {
        let exhausted = frames.last().is_some_and(Frame::is_exhausted);
        if let (true, Some(tip)) = (exhausted, self.path.last_mut()) {
            tip.applied.state_json = serde_json::Value::Null;
        }
    }

    fn apply(&mut self, row: &Row<'a>) -> Option<AppliedState> {
        let tip = self.path.last()?;
        match self
            .client
            .apply_delta(&tip.applied.state_json, &row.delta.delta_payload)
        {
            Ok(applied) => Some(applied),
            Err(error) => {
                tracing::debug!(nonce = row.delta.nonce, %error, "self-heal: a row did not apply");
                if row.is_canonical() {
                    self.canonical_link_failed = true;
                }
                None
            }
        }
    }

    fn path_holds_nonce(&self, nonce: u64) -> bool {
        self.path.iter().any(|step| step.row.delta.nonce == nonce)
    }

    fn keep_if_deepest(&mut self) {
        if self.path.len() > self.deepest.len() {
            self.deepest = self.path.iter().map(without_blob).collect();
        }
    }
}

fn without_blob<'a>(step: &Step<'a>) -> Step<'a> {
    Step {
        row: step.row,
        applied: AppliedState {
            state_json: serde_json::Value::Null,
            commitment: step.applied.commitment.clone(),
            nonce: step.applied.nonce,
            cosigners: step.applied.cosigners.clone(),
            guardian_commitment: step.applied.guardian_commitment.clone(),
        },
    }
}
