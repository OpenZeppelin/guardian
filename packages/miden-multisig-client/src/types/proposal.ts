import type { ProposalType as PsmProposalType } from '@openzeppelin/psm-client';
export type ProposalType = Exclude<PsmProposalType, 'custom'>;

export type ProposalStatus = 'pending' | 'ready' | 'finalized';

export interface ProposalSignatureEntry {
  signerId: string;
  signature: { scheme: 'falcon'; signature: string };
  timestamp: string;
}

interface BaseProposalMetadata {
  proposalType: ProposalType;
  description: string;
  saltHex?: string;
}

export interface UpdateSignersProposalMetadata extends BaseProposalMetadata {
  proposalType: 'add_signer' | 'remove_signer' | 'change_threshold';
  targetThreshold: number;
  targetSignerCommitments: string[];
}

export interface SwitchPsmProposalMetadata extends BaseProposalMetadata {
  proposalType: 'switch_psm';
  newPsmPubkey: string;
  newPsmEndpoint?: string;
  targetThreshold?: number;
  targetSignerCommitments?: string[];
}

export interface ConsumeNotesProposalMetadata extends BaseProposalMetadata {
  proposalType: 'consume_notes';
  noteIds: string[];
}

export interface P2IdProposalMetadata extends BaseProposalMetadata {
  proposalType: 'p2id';
  recipientId: string;
  faucetId: string;
  amount: string;
}

export interface UnknownProposalMetadata extends BaseProposalMetadata {
  proposalType: 'unknown';
}

export type ProposalMetadata =
  | UpdateSignersProposalMetadata
  | SwitchPsmProposalMetadata
  | ConsumeNotesProposalMetadata
  | P2IdProposalMetadata
  | UnknownProposalMetadata;

export interface Proposal {
  id: string;
  accountId: string;
  nonce: number;
  status: ProposalStatus;
  txSummary: string;
  signatures: ProposalSignatureEntry[];
  metadata: ProposalMetadata;
}

export interface ExportedProposal {
  accountId: string;
  nonce: number;
  commitment: string;
  txSummaryBase64: string;
  signatures: Array<{
    commitment: string;
    signatureHex: string;
    timestamp?: string;
  }>;
  metadata?: ProposalMetadata;
}
