import type { Proposal, ConsumableNote } from '@openzeppelin/miden-multisig-client';

export interface ProposalView {
  id: string;
  kind?: string;
  description?: string;
  status: Proposal['status'];
  createdAt?: string;
}

export function toProposalView(proposal: Proposal): ProposalView {
  return {
    id: proposal.id,
    kind: proposal.metadata?.kind ?? proposal.metadata?.proposalType,
    description: proposal.metadata?.description,
    status: proposal.status,
    createdAt: proposal.metadata?.saltHex ? undefined : undefined,
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

