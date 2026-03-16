import type { Account } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '@openzeppelin/psm-client';
import type { ProcedureName } from './procedures.js';
import type { AccountState } from './multisig.js';
import type { DetectedMultisigConfig } from './inspector.js';
import type { TransactionProposal } from './types/proposal.js';

export type {
  Signer,
  FalconSignature,
  EcdsaSignature,
  ProposalSignature,
  SignatureScheme,
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
  ExportedTransactionProposal,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  ProposalStatus,
  ProposalType,
  SignTransactionProposalParams,
  TransactionProposal,
  TransactionProposalSignature,
  TransactionProposalStatus,
} from './types/proposal.js';

export interface SyncResult {
  proposals: TransactionProposal[];
  state: AccountState;
  notes: ConsumableNote[];
  config: DetectedMultisigConfig;
}

export interface TransactionProposalResult {
  proposal: TransactionProposal;
  proposals: TransactionProposal[];
}

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
  psmPublicKey?: string;
  psmEnabled?: boolean;
  storageMode?: 'private' | 'public';
  procedureThresholds?: ProcedureThreshold[];
  signatureScheme?: SignatureScheme;
}

export interface CreateAccountResult {
  account: Account;
  seed: Uint8Array;
}

export type TransactionType =
  | { type: 'p2id'; recipient: string; faucetId: string; amount: bigint }
  | { type: 'consumeNotes'; noteIds: string[] }
  | { type: 'updateSigners'; newThreshold: number; newSignerCommitments: string[] }
  | { type: 'updateProcedureThreshold'; procedure: ProcedureName; threshold: number };

export interface NoteAsset {
  faucetId: string;
  amount: bigint;
}

export interface ConsumableNote {
  id: string;
  assets: NoteAsset[];
}
