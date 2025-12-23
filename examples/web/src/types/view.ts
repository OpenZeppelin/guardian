import type { Proposal, ConsumableNote } from '@openzeppelin/miden-multisig-client';

export interface ProposalView {
  id: string;
  proposalType?: string;
  description?: string;
  status: Proposal['status'];
  createdAt?: string;
}

export function toProposalView(proposal: Proposal): ProposalView {
  const meta = proposal.metadata as { proposalType: string; description?: string } | undefined;
  return {
    id: proposal.id,
    proposalType: meta?.proposalType,
    description: meta?.description,
    status: proposal.status,
    createdAt: undefined,
  };
}

export interface ConsumableNoteView {
  id: string;
  assets: ConsumableNote['assets'];
}

export function toConsumableNoteView(note: ConsumableNote): ConsumableNoteView {
  return {
    id: note.id,
    assets: note.assets,
  };
}
