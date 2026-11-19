//! The recovery-file entry of an account the planner could not prove.

use crate::delta_object::{DeltaObject, DeltaStatus};
use crate::network::AppliedState;
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use crate::storage::ProposalRecord;

use super::replay::Step;
use super::rows::Rows;
use super::types::{
    AccountInputs, AccountPlan, ChainInfo, ChainRead, CreationDeltaInfo, DeltaRowInfo, NeededInfo,
    ProposalInfo, RecoveryReason, ReplayInfo, StoredInfo, UnrecoveredAccount,
};

/// What the planner knows about an account it could not prove.
pub(super) struct Recovery<'s, 'a> {
    pub reason: RecoveryReason,
    pub detail: Option<String>,
    /// The creation row and the account it builds.
    pub creation: Option<&'s Step<'a>>,
    /// The replay path, from the creation row to where it stopped.
    pub trace: &'s [Step<'a>],
    pub needed: Option<String>,
}

impl Recovery<'_, '_> {
    pub fn into_plan(self, inputs: &AccountInputs, rows: &Rows<'_>) -> AccountPlan {
        AccountPlan::Unrecovered(Box::new(UnrecoveredAccount {
            account_id: inputs.metadata.account_id.clone(),
            reason: self.reason,
            detail: self.detail.clone(),
            chain: chain_info(&inputs.chain),
            stored: stored_info(inputs),
            creation_delta: creation_info(self.creation),
            replay: replay_info(self.trace),
            needed: NeededInfo {
                state_with_commitment: self.needed.clone(),
            },
            delta_rows: delta_rows(rows, self.trace),
            proposals: inputs.proposals.iter().map(proposal_info).collect(),
            pause: pauses(self.reason),
            release: false,
        }))
    }
}

/// Every reason pauses the account except a failed chain read. A
/// `switch_guardian` row does not release or unpause: an attacker that
/// was a stored cosigner can create one.
fn pauses(reason: RecoveryReason) -> bool {
    match reason {
        RecoveryReason::RpcError => false,
        RecoveryReason::NoBase
        | RecoveryReason::CreationDeltaPending
        | RecoveryReason::ReplayShort
        | RecoveryReason::LeftThisGuardian
        | RecoveryReason::AmbiguousHistory
        | RecoveryReason::UndecodableRow => true,
    }
}

fn chain_info(chain: &ChainRead) -> ChainInfo {
    match chain {
        ChainRead::Mismatch { commitment } => ChainInfo {
            status: "mismatch".to_string(),
            commitment: Some(commitment.clone()),
        },
        ChainRead::Absent => ChainInfo {
            status: "absent".to_string(),
            commitment: None,
        },
    }
}

fn stored_info(inputs: &AccountInputs) -> StoredInfo {
    StoredInfo {
        commitment: inputs.stored.commitment.clone(),
        nonce: inputs.stored.nonce,
        signers: sorted(inputs.metadata.auth.cosigner_commitments()),
        created_at: inputs.stored.created_at.clone(),
        updated_at: inputs.stored.updated_at.clone(),
    }
}

fn creation_info(creation: Option<&Step<'_>>) -> CreationDeltaInfo {
    match creation {
        Some(step) => CreationDeltaInfo {
            status: step.row.status_label().to_string(),
            nonce: Some(step.row.delta.nonce),
            new_commitment: Some(step.applied.commitment.clone()),
            signers: Some(signers_of(&step.applied)),
        },
        None => CreationDeltaInfo {
            status: "none".to_string(),
            nonce: None,
            new_commitment: None,
            signers: None,
        },
    }
}

fn replay_info(trace: &[Step<'_>]) -> ReplayInfo {
    let last = trace.last();
    ReplayInfo {
        base: if trace.is_empty() {
            "none"
        } else {
            "creation_delta"
        }
        .to_string(),
        rows_applied: trace.len(),
        last_commitment: last.map(|step| step.applied.commitment.clone()),
        last_nonce: last.and_then(|step| step.applied.nonce),
        last_signers: last.map(|step| signers_of(&step.applied)),
    }
}

fn delta_rows(rows: &Rows<'_>, trace: &[Step<'_>]) -> Vec<DeltaRowInfo> {
    let mut infos: Vec<DeltaRowInfo> = rows
        .all()
        .iter()
        .map(|row| DeltaRowInfo {
            nonce: row.delta.nonce,
            status: row.status_label().to_string(),
            prev_commitment: row.delta.prev_commitment.clone(),
            new_commitment: row.delta.new_commitment.clone(),
            proposal_type: row.delta.proposal_type().map(str::to_string),
            applied: trace.iter().any(|step| step.row.source == row.source),
        })
        .collect();
    infos.sort_by(|left, right| (left.nonce, &left.status).cmp(&(right.nonce, &right.status)));
    infos
}

fn proposal_info(record: &ProposalRecord) -> ProposalInfo {
    let proposal = &record.proposal;
    ProposalInfo {
        id: record.commitment.clone(),
        nonce: proposal.nonce,
        base: proposal.prev_commitment.clone(),
        proposer: proposer_of(proposal),
        proposal_type: proposal.proposal_type().map(str::to_string),
        verified_signers: verified_signers(proposal),
    }
}

fn proposer_of(proposal: &DeltaObject) -> Option<String> {
    match &proposal.status {
        DeltaStatus::Pending { proposer_id, .. } => Some(proposer_id.clone()),
        DeltaStatus::Candidate { .. }
        | DeltaStatus::Canonical { .. }
        | DeltaStatus::Retained { .. }
        | DeltaStatus::Discarded { .. } => None,
    }
}

/// The signer of every stored signature that verifies over the proposal.
fn verified_signers(proposal: &DeltaObject) -> Vec<String> {
    let DeltaStatus::Pending { cosigner_sigs, .. } = &proposal.status else {
        return Vec::new();
    };
    let Ok(summary) = proposal_tx_summary(&proposal.delta_payload) else {
        return Vec::new();
    };
    cosigner_sigs
        .iter()
        .filter_map(|signature| verify_proposal_signature(&summary, &signature.signature).ok())
        .collect()
}

/// The sorted signer commitments of a state.
pub(super) fn signers_of(state: &AppliedState) -> Vec<String> {
    sorted(
        state
            .cosigners
            .iter()
            .map(|cosigner| cosigner.commitment.clone())
            .collect(),
    )
}

fn sorted(mut commitments: Vec<String>) -> Vec<String> {
    commitments.sort();
    commitments
}
