import type { ProposalType as PsmProposalType, SignatureScheme } from '@openzeppelin/psm-client';
export type ProposalType = Exclude<PsmProposalType, 'custom'>;

export type TransactionProposalStatus =
  | { type: 'pending'; signaturesCollected: number; signaturesRequired: number; signers: string[] }
  | { type: 'ready' }
  | { type: 'finalized' };

export interface TransactionProposalSignature {
  signerId: string;
  signature: { scheme: 'falcon' | 'ecdsa'; signature: string; publicKey?: string };
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

export interface TransactionProposal {
  id: string;
  commitment: string;
  accountId: string;
  nonce: number;
  status: TransactionProposalStatus;
  txSummary: string;
  signatures: TransactionProposalSignature[];
  metadata: ProposalMetadata;
}

export interface ExportedTransactionProposal {
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

export interface SignTransactionProposalParams {
  commitment: string;
  signature: string;
  publicKey?: string;
  scheme?: SignatureScheme;
}
