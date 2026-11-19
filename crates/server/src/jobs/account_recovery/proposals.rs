use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::metadata::auth::ChainCosigner;
use crate::network::AppliedState;
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use crate::storage::ProposalRecord;
use crate::storage::postgres::recovery::{ProposalChange, ReviewedProposal};
use miden_protocol::transaction::TransactionSummary;

/// Reviews an account's pending proposals against the signer set of its restored state. A
/// proposal none of those signers proposed or approved is removed, and a proposal that stays
/// keeps only their approvals. A proposal whose transaction summary cannot be read stays as
/// it is.
pub(super) fn review(restored: &AppliedState, pending: &[ProposalRecord]) -> Vec<ReviewedProposal> {
    pending
        .iter()
        .map(|record| ReviewedProposal {
            commitment: record.commitment.clone(),
            status: record.proposal.status.clone(),
            change: change_for(&restored.cosigners, &record.proposal),
        })
        .collect()
}

fn change_for(signers: &[ChainCosigner], proposal: &DeltaObject) -> ProposalChange {
    let (timestamp, proposer_id, cosigner_sigs) = match &proposal.status {
        DeltaStatus::Pending {
            timestamp,
            proposer_id,
            cosigner_sigs,
        } => (timestamp, proposer_id, cosigner_sigs),
        DeltaStatus::Candidate { .. }
        | DeltaStatus::Canonical { .. }
        | DeltaStatus::Retained { .. }
        | DeltaStatus::Discarded { .. } => return ProposalChange::Keep,
    };
    let Ok(summary) = proposal_tx_summary(&proposal.delta_payload) else {
        return ProposalChange::Keep;
    };
    let approvals = approvals_of(signers, &summary, cosigner_sigs);
    let proposed_by_signer = signers
        .iter()
        .any(|signer| signer.commitment.eq_ignore_ascii_case(proposer_id));
    if approvals.is_empty() && !proposed_by_signer {
        return ProposalChange::Remove;
    }
    if approvals.len() == cosigner_sigs.len() {
        return ProposalChange::Keep;
    }
    ProposalChange::DropApprovals {
        delta_payload: payload_with_approvals(&proposal.delta_payload, &approvals),
        status: DeltaStatus::Pending {
            timestamp: timestamp.clone(),
            proposer_id: proposer_id.clone(),
            cosigner_sigs: approvals,
        },
    }
}

fn approvals_of(
    signers: &[ChainCosigner],
    summary: &TransactionSummary,
    signatures: &[CosignerSignature],
) -> Vec<CosignerSignature> {
    signatures
        .iter()
        .filter(|approval| {
            verify_proposal_signature(summary, &approval.signature).is_ok_and(|signer| {
                signers.iter().any(|known| {
                    known.commitment.eq_ignore_ascii_case(&signer)
                        && known.scheme == Some(approval.signature.scheme())
                })
            })
        })
        .cloned()
        .collect()
}

/// The payload with its `signatures` list reduced to the signers of `approvals`. A proposal
/// stores each approval in its status and in its payload, and clients read either.
fn payload_with_approvals(
    delta_payload: &serde_json::Value,
    approvals: &[CosignerSignature],
) -> serde_json::Value {
    let mut payload = delta_payload.clone();
    if let Some(signatures) = payload
        .get_mut("signatures")
        .and_then(serde_json::Value::as_array_mut)
    {
        signatures.retain(|entry| {
            entry
                .get("signer_id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|signer| {
                    approvals
                        .iter()
                        .any(|approval| approval.signer_id.eq_ignore_ascii_case(signer))
                })
        });
    }
    payload
}
