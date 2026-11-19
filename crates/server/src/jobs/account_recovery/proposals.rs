use crate::delta_object::{CosignerSignature, DeltaObject, DeltaStatus};
use crate::metadata::auth::ChainCosigner;
use crate::network::AppliedState;
use crate::services::proposal_signature::{proposal_tx_summary, verify_proposal_signature};
use crate::storage::ProposalRecord;
use crate::storage::postgres::recovery::{ProposalChange, ReviewedProposal};

/// Reviews an account's pending proposals against the signer set of its restored state. A
/// proposal none of those signers proposed or approved is removed, and a proposal that stays
/// keeps only their approvals.
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

fn change_for(signers: &[ChainCosigner], proposal: &DeltaObject) -> Option<ProposalChange> {
    let DeltaStatus::Pending {
        timestamp,
        proposer_id,
        cosigner_sigs,
    } = &proposal.status
    else {
        return None;
    };
    let approvals = approvals_of(signers, proposal, cosigner_sigs);
    let proposed_by_signer = signers
        .iter()
        .any(|signer| signer.commitment.eq_ignore_ascii_case(proposer_id));
    if approvals.is_empty() && !proposed_by_signer {
        return Some(ProposalChange::Remove);
    }
    (approvals.len() != cosigner_sigs.len()).then(|| {
        ProposalChange::DropApprovals(DeltaStatus::Pending {
            timestamp: timestamp.clone(),
            proposer_id: proposer_id.clone(),
            cosigner_sigs: approvals,
        })
    })
}

fn approvals_of(
    signers: &[ChainCosigner],
    proposal: &DeltaObject,
    signatures: &[CosignerSignature],
) -> Vec<CosignerSignature> {
    let Ok(summary) = proposal_tx_summary(&proposal.delta_payload) else {
        return Vec::new();
    };
    signatures
        .iter()
        .filter(|approval| {
            verify_proposal_signature(&summary, &approval.signature).is_ok_and(|signer| {
                signers.iter().any(|known| {
                    known.commitment.eq_ignore_ascii_case(&signer)
                        && known.scheme == Some(approval.signature.scheme())
                })
            })
        })
        .cloned()
        .collect()
}
