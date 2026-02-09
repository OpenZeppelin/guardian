use miden_protocol::Word;

use crate::error::Result;
use crate::proposal::{Proposal, TransactionType};

pub(crate) fn signer_commitments_for_transaction(proposal: &Proposal) -> Result<Option<Vec<Word>>> {
    if matches!(
        &proposal.transaction_type,
        TransactionType::AddCosigner { .. }
            | TransactionType::RemoveCosigner { .. }
            | TransactionType::UpdateSigners { .. }
    ) {
        Ok(Some(proposal.metadata.signer_commitments()?))
    } else {
        Ok(proposal.metadata.signer_commitments().ok())
    }
}
