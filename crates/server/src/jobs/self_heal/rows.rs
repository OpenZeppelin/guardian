//! The delta rows of one account as the planner reads them: the rows of
//! the `deltas` table and the archived superseded rows, in one list.

use std::collections::HashMap;

use crate::delta_object::{DeltaObject, DeltaStatus};

use super::types::AccountInputs;

/// Where a delta row is stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum RowSource {
    /// Index into [`AccountInputs::deltas`].
    Deltas(usize),
    /// Index into [`AccountInputs::superseded`].
    Superseded(usize),
}

/// One delta row and where it is stored.
#[derive(Debug, Clone, Copy)]
pub(super) struct Row<'a> {
    pub source: RowSource,
    pub delta: &'a DeltaObject,
}

impl Row<'_> {
    /// The order in which the replay tries rows that share one base.
    fn replay_rank(&self) -> u8 {
        match self.source {
            RowSource::Superseded(_) => 4,
            RowSource::Deltas(_) => match &self.delta.status {
                DeltaStatus::Canonical { .. } => 0,
                DeltaStatus::Candidate { .. } => 1,
                DeltaStatus::Retained { .. } => 2,
                DeltaStatus::Discarded { .. } => 3,
                DeltaStatus::Pending { .. } => 5,
            },
        }
    }

    /// True for a canonical row of the `deltas` table.
    pub fn is_canonical(&self) -> bool {
        matches!(self.source, RowSource::Deltas(_)) && self.delta.status.is_canonical()
    }

    /// The status name the recovery file uses.
    pub fn status_label(&self) -> &'static str {
        match self.source {
            RowSource::Superseded(_) => "superseded",
            RowSource::Deltas(_) => match &self.delta.status {
                DeltaStatus::Canonical { .. } => "canonical",
                DeltaStatus::Candidate { .. } => "candidate",
                DeltaStatus::Retained { .. } => "retained",
                DeltaStatus::Discarded { .. } => "discarded",
                DeltaStatus::Pending { .. } => "pending",
            },
        }
    }

    fn sort_key(&self) -> (u8, u64) {
        (self.replay_rank(), self.delta.nonce)
    }
}

/// Every delta row of one account, indexed by base commitment.
pub(super) struct Rows<'a> {
    all: Vec<Row<'a>>,
    by_base: HashMap<String, Vec<Row<'a>>>,
}

impl<'a> Rows<'a> {
    pub fn new(inputs: &'a AccountInputs) -> Self {
        let deltas = inputs.deltas.iter().enumerate().map(|(index, delta)| Row {
            source: RowSource::Deltas(index),
            delta,
        });
        let superseded = inputs
            .superseded
            .iter()
            .enumerate()
            .map(|(index, archived)| Row {
                source: RowSource::Superseded(index),
                delta: &archived.delta,
            });
        let mut all: Vec<Row<'a>> = deltas.chain(superseded).collect();
        all.sort_by_key(Row::sort_key);
        let mut by_base: HashMap<String, Vec<Row<'a>>> = HashMap::new();
        for row in &all {
            by_base
                .entry(commitment_key(&row.delta.prev_commitment))
                .or_default()
                .push(*row);
        }
        Self { all, by_base }
    }

    /// Every row, canonical rows first, then by status and nonce.
    pub fn all(&self) -> &[Row<'a>] {
        &self.all
    }

    /// The rows whose stored base is `commitment`, in replay order.
    pub fn based_on(&self, commitment: &str) -> &[Row<'a>] {
        self.by_base
            .get(&commitment_key(commitment))
            .map_or(&[], Vec::as_slice)
    }

    /// The stored base of the row with the lowest nonce.
    pub fn lowest_nonce_base(&self) -> Option<&'a str> {
        self.all
            .iter()
            .min_by_key(|row| row.delta.nonce)
            .map(|row| row.delta.prev_commitment.as_str())
    }
}

/// The form two commitments are compared in.
pub(super) fn commitment_key(commitment: &str) -> String {
    commitment.to_ascii_lowercase()
}

/// Whether two commitments name the same state.
pub(super) fn same_commitment(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}
