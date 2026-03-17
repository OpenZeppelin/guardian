import type {
  ProposalSignature,
  ProposalType as PsmProposalType,
  SignatureScheme,
} from '@openzeppelin/psm-client';
import type { ProcedureName } from '../procedures.js';

export type ProposalType = Exclude<PsmProposalType, 'custom'>;

export type ProposalStatus = 'pending' | 'ready' | 'finalized';

export type TransactionProposalStatus =
  | { type: 'pending'; signaturesCollected: number; signaturesRequired: number; signers: string[] }
  | { type: 'ready' }
  | { type: 'finalized' };

export interface ProposalSignatureEntry {
  signerId: string;
  signature: ProposalSignature;
  timestamp: string;
}

export type TransactionProposalSignature = ProposalSignatureEntry;

interface BaseProposalMetadata {
  proposalType: ProposalType;
  description: string;
  saltHex?: string;
  requiredSignatures?: number;
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

export interface UpdateProcedureThresholdProposalMetadata extends BaseProposalMetadata {
  proposalType: 'update_procedure_threshold';
  targetProcedure: ProcedureName;
  targetThreshold: number;
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
  | UpdateProcedureThresholdProposalMetadata
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

export interface ExportedProposal {
  accountId: string;
  nonce: number;
  commitment: string;
  txSummaryBase64: string;
  signatures: Array<{
    commitment: string;
    signatureHex: string;
    scheme?: SignatureScheme;
    publicKey?: string;
    timestamp?: string;
  }>;
  metadata: ProposalMetadata;
}

export type ExportedTransactionProposal = ExportedProposal;

export interface SignTransactionProposalParams {
  commitment: string;
  signature: string;
  publicKey?: string;
  scheme?: SignatureScheme;
}
