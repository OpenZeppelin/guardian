import type {
  ProposalSignature,
  ProposalType as GuardianProposalType,
  SignatureScheme,
} from '@openzeppelin/guardian-client';
import type { ProcedureName } from '../procedures.js';

export type ProposalType = Exclude<GuardianProposalType, 'custom'>;

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

export interface SwitchGuardianProposalMetadata extends BaseProposalMetadata {
  proposalType: 'switch_guardian';
  newGuardianPubkey: string;
  newGuardianEndpoint?: string;
  targetThreshold?: number;
  targetSignerCommitments?: string[];
}

export interface UpdateProcedureThresholdProposalMetadata extends BaseProposalMetadata {
  proposalType: 'update_procedure_threshold';
  targetProcedure: ProcedureName;
  targetThreshold: number;
}

/** `consume_notes` metadata version. Absence on the wire => v1 (issue #229). */
export const CONSUME_NOTES_METADATA_VERSION_V2 = 2 as const;

/** Max serialized v2 metadata, enforced at creation (FR-011). */
export const MAX_CONSUME_NOTES_METADATA_BYTES = 256 * 1024;

export interface ConsumeNotesProposalMetadata extends BaseProposalMetadata {
  proposalType: 'consume_notes';
  noteIds: string[];
  /** Absent or `1` => v1 (legacy), `2` => v2 (issue #229). */
  metadataVersion?: 1 | 2;
  /** v2: base64-encoded `note.serialize()` output, index-aligned with `noteIds`. */
  notes?: string[];
}

export function isConsumeNotesV2(md: ConsumeNotesProposalMetadata): boolean {
  return md.metadataVersion === CONSUME_NOTES_METADATA_VERSION_V2;
}

export function isConsumeNotesV1(md: ConsumeNotesProposalMetadata): boolean {
  return md.metadataVersion === undefined || md.metadataVersion === 1;
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
  | SwitchGuardianProposalMetadata
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
