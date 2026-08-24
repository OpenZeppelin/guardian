import type { ProposalSignature, SignatureScheme } from '@openzeppelin/guardian-client';
import type { ProcedureName } from '../procedures.js';

/**
 * Closed set of proposal types the multisig SDK models behaviorally, plus the
 * `'custom'` bucket for any server-defined type the SDK does not model (issue
 * #266). Defined explicitly (not derived from the now-arbitrary guardian-client
 * wire union) so the exhaustive switches in the metadata codec stay sound.
 */
export type ProposalType =
  | 'add_signer'
  | 'remove_signer'
  | 'change_threshold'
  | 'update_procedure_threshold'
  | 'switch_guardian'
  | 'consume_notes'
  | 'p2id'
  | 'custom';

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
  /**
   * Base64-serialized Miden `ChainAnchor` pinning the reference block the
   * proposal's transaction summary was built at. Required to verify or
   * execute the proposal: since protocol 0.16 the signed summary binds the
   * reference block commitment, so it only reproduces when re-executed at
   * that block.
   */
  chainAnchor?: string;
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

/** Wire values for a P2ID note's visibility (issue #322). */
export type P2idNoteVisibility = 'public' | 'private';

export function isP2idNoteVisibility(value: string): value is P2idNoteVisibility {
  return value === 'public' || value === 'private';
}

/** Maximum P2IDE block height: heights are `u32` on-chain (`BlockNumber`). */
export const MAX_P2IDE_BLOCK_HEIGHT = 0xffff_ffff;

/**
 * Validates a P2IDE reclaim/timelock height (issue #366). Heights are `u32`
 * block numbers; `0` is rejected because it is the on-chain encoding for "no
 * constraint", so accepting it would silently build an unconstrained note.
 * An invalid value throws rather than being dropped, which would rebuild a
 * note that could never match the signed tx_summary commitment.
 */
export function parseP2ideHeight(
  field: 'reclaimHeight' | 'timelockHeight',
  value: number | undefined,
): number | undefined {
  if (value === undefined) {
    return undefined;
  }
  if (!Number.isInteger(value) || value < 1 || value > MAX_P2IDE_BLOCK_HEIGHT) {
    throw new Error(
      `unsupported ${field} '${value}': expected an integer between 1 and ${MAX_P2IDE_BLOCK_HEIGHT}`,
    );
  }
  return value;
}

export interface P2IdProposalMetadata extends BaseProposalMetadata {
  proposalType: 'p2id';
  recipientId: string;
  faucetId: string;
  amount: string;
  /** Visibility of the created note. Absent on the wire => 'public' (pre-#322 proposals). */
  noteType?: P2idNoteVisibility;
  /**
   * Absolute block height at which the sender may reclaim the note (issue
   * #366). Presence of either height means the proposal creates a P2IDE note
   * instead of a plain P2ID note; both absent => plain P2ID (pre-#366
   * proposals).
   */
  reclaimHeight?: number;
  /** Absolute block height before which the note cannot be consumed. */
  timelockHeight?: number;
}

export interface CustomProposalMetadata extends BaseProposalMetadata {
  proposalType: 'custom';
  /** Original server-defined proposal label, preserved during round trips. */
  rawProposalType: string;
}

export type ProposalMetadata =
  | UpdateSignersProposalMetadata
  | SwitchGuardianProposalMetadata
  | UpdateProcedureThresholdProposalMetadata
  | ConsumeNotesProposalMetadata
  | P2IdProposalMetadata
  | CustomProposalMetadata;

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
