import type { ProposalType as PsmProposalType } from '@openzeppelin/psm-client';

export type ProposalKind =
  | 'add_signer'
  | 'remove_signer'
  | 'change_threshold'
  | 'switch_psm'
  | 'consume_notes'
  | 'p2id';

export type ProposalStatus =
  | { type: 'pending'; signaturesCollected: number; signaturesRequired: number; signers: string[] }
  | { type: 'ready' }
  | { type: 'finalized' };

export interface ProposalSignatureEntry {
  signerId: string;
  signature: { scheme: 'falcon'; signature: string };
  timestamp: string;
}

interface BaseProposalMetadata {
  kind: ProposalKind;
  description?: string;
  saltHex?: string;
}

export interface UpdateSignersProposalMetadata extends BaseProposalMetadata {
  kind: 'add_signer' | 'remove_signer' | 'change_threshold';
  targetThreshold: number;
  targetSignerCommitments: string[];
}

export interface SwitchPsmProposalMetadata extends BaseProposalMetadata {
  kind: 'switch_psm';
  newPsmPubkey: string;
  newPsmEndpoint?: string;
  targetThreshold?: number;
  targetSignerCommitments?: string[];
}

export interface ConsumeNotesProposalMetadata extends BaseProposalMetadata {
  kind: 'consume_notes';
  noteIds: string[];
}

export interface P2IdProposalMetadata extends BaseProposalMetadata {
  kind: 'p2id';
  recipientId: string;
  faucetId: string;
  amount: string;
}

export type ProposalMetadata =
  | UpdateSignersProposalMetadata
  | SwitchPsmProposalMetadata
  | ConsumeNotesProposalMetadata
  | P2IdProposalMetadata;

export interface Proposal {
  id: string;
  accountId: string;
  nonce: number;
  status: ProposalStatus;
  txSummary: string;
  signatures: ProposalSignatureEntry[];
  metadata?: ProposalMetadata;
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

