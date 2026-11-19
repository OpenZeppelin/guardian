//! Verdicts for the pending proposals of one account once the true state
//! is proven. A proposal is kept only when a true signer signed it and it
//! still applies to a state the account can reach.

use miden_protocol::transaction::TransactionSummary;

use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::metadata::auth::ChainCosigner;
use crate::network::{AppliedState, NetworkClient};
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use crate::storage::{HealProposalWrite, ProposalRecord};

use super::deltas::Frontier;
use super::types::{RowAction, RowKind, RowVerdict};

/// The proposal writes and log entries of one account.
#[derive(Default)]
pub(super) struct ProposalVerdicts {
    pub writes: Vec<HealProposalWrite>,
    pub actions: Vec<RowAction>,
}

/// Judges the pending proposals of one account.
pub(super) struct ProposalJudge<'c> {
    pub client: &'c dyn NetworkClient,
    pub frontier: &'c Frontier,
    pub true_state: &'c AppliedState,
}

enum Verdict {
    Keep,
    KeepWithFewer(Vec<CosignerSignature>),
    Delete(&'static str),
}

impl ProposalJudge<'_> {
    pub fn judge(&self, proposals: &[ProposalRecord]) -> ProposalVerdicts {
        let mut verdicts = ProposalVerdicts::default();
        for record in proposals {
            let DeltaStatus::Pending { cosigner_sigs, .. } = &record.proposal.status else {
                continue;
            };
            let verdict = self.verdict(&record.proposal, cosigner_sigs);
            record_verdict(record, verdict, &mut verdicts);
        }
        verdicts
    }

    fn verdict(&self, proposal: &DeltaObject, signatures: &[CosignerSignature]) -> Verdict {
        let Ok(summary) = proposal_tx_summary(&proposal.delta_payload) else {
            return Verdict::Delete("its transaction summary does not decode");
        };
        let base = self.frontier.get(&proposal.prev_commitment);
        let signers = &base.unwrap_or(self.true_state).cosigners;
        let valid: Vec<CosignerSignature> = signatures
            .iter()
            .filter(|signature| true_signer_of(&summary, signature, signers).is_some())
            .cloned()
            .collect();
        if valid.is_empty() {
            return Verdict::Delete("no signature of a true signer");
        }
        let Some(base) = base else {
            return Verdict::Delete("its base is neither the proven state nor a kept candidate");
        };
        if !self.applies_to(base, proposal) {
            return Verdict::Delete("it does not apply to its base state");
        }
        if valid.len() == signatures.len() {
            Verdict::Keep
        } else {
            Verdict::KeepWithFewer(valid)
        }
    }

    fn applies_to(&self, base: &AppliedState, proposal: &DeltaObject) -> bool {
        let Some(tx_summary) = proposal.delta_payload.get("tx_summary") else {
            return false;
        };
        self.client
            .verify_delta(&base.commitment, &base.state_json, tx_summary)
            .is_ok()
    }
}

fn record_verdict(record: &ProposalRecord, verdict: Verdict, verdicts: &mut ProposalVerdicts) {
    let commitment = record.commitment.clone();
    let action = |verdict: RowVerdict, reason: &str| RowAction {
        row: RowKind::Proposal,
        id: commitment.clone(),
        verdict,
        reason: reason.to_string(),
    };
    match verdict {
        Verdict::Keep => verdicts.actions.push(action(
            RowVerdict::Keep,
            "signed by a true signer on a reachable base",
        )),
        Verdict::KeepWithFewer(valid) => {
            verdicts.writes.push(HealProposalWrite::Put {
                commitment: commitment.clone(),
                proposal: with_signatures(&record.proposal, valid),
            });
            verdicts.actions.push(action(
                RowVerdict::KeepWithFewerSignatures,
                "signatures that are not of a true signer were removed",
            ));
        }
        Verdict::Delete(reason) => {
            verdicts.writes.push(HealProposalWrite::Delete {
                commitment: commitment.clone(),
            });
            verdicts.actions.push(action(RowVerdict::Delete, reason));
        }
    }
}

fn with_signatures(proposal: &DeltaObject, signatures: Vec<CosignerSignature>) -> DeltaObject {
    let status = match &proposal.status {
        DeltaStatus::Pending {
            timestamp,
            proposer_id,
            ..
        } => DeltaStatus::Pending {
            timestamp: timestamp.clone(),
            proposer_id: proposer_id.clone(),
            cosigner_sigs: signatures,
        },
        other @ (DeltaStatus::Candidate { .. }
        | DeltaStatus::Canonical { .. }
        | DeltaStatus::Retained { .. }
        | DeltaStatus::Discarded { .. }) => other.clone(),
    };
    DeltaObject {
        status,
        ..proposal.clone()
    }
}

/// The signer of `signature` when it verifies over `summary`, its signer
/// is one of `signers`, and it is made under the scheme the state stores
/// for that signer. This mirrors the push gate's approver count.
pub(super) fn true_signer_of(
    summary: &TransactionSummary,
    signature: &CosignerSignature,
    signers: &[ChainCosigner],
) -> Option<String> {
    let signer = verify_proposal_signature(summary, &signature.signature).ok()?;
    let approver = signers
        .iter()
        .find(|approver| approver.commitment.eq_ignore_ascii_case(&signer))?;
    (approver.scheme == Some(signature.signature.scheme())).then_some(signer)
}
