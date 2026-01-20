import type { Account } from '@demox-labs/miden-sdk';
import type { ProcedureName } from './procedures.js';

export type {
  Signer,
  FalconSignature,
  CosignerSignature,
  AuthConfig,
  DeltaStatus,
  DeltaObject,
  StateObject,
  ConfigureRequest,
  ConfigureResponse,
  PubkeyResponse,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ProposalsResponse,
  SignProposalRequest,
} from '@openzeppelin/psm-client';

export type {
  ExportedProposal,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  ProposalStatus,
  ProposalType,
} from './types/proposal.js';

export interface MultisigAccountState {
  id: string;
  nonce: number;
  threshold: number;
  cosignerCommitments: string[];
}

/**
 * Per-procedure threshold override.
 *
 * @example
 * ```typescript
 * const thresholds: ProcedureThreshold[] = [
 *   { procedure: 'receive_asset', threshold: 1 },
 *   { procedure: 'update_signers', threshold: 3 },
 * ];
 * ```
 */
export interface ProcedureThreshold {
  procedure: ProcedureName;
  /** Threshold for this procedure (1 to numSigners) */
  threshold: number;
}

export interface MultisigConfig {
  threshold: number;
  signerCommitments: string[];
  psmCommitment: string;
  psmEnabled?: boolean;
  storageMode?: 'private' | 'public';
  procedureThresholds?: ProcedureThreshold[];
}

export interface CreateAccountResult {
  account: Account;
  seed: Uint8Array;
}

export type TransactionType =
  | { type: 'p2id'; recipient: string; faucetId: string; amount: bigint }
  | { type: 'consumeNotes'; noteIds: string[] }
  | { type: 'updateSigners'; newThreshold: number; newSignerCommitments: string[] };

export interface NoteAsset {
  faucetId: string;
  amount: bigint;
}

export interface ConsumableNote {
  id: string;
  assets: NoteAsset[];
}
