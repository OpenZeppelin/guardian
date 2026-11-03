import type { Account } from '@miden-sdk/miden-sdk';
import type { SignatureScheme } from '@openzeppelin/guardian-client';
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
} from '@openzeppelin/guardian-client';

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

/**
 * An approver together with the signature scheme the account verifies its
 * signatures under.
 */
export interface SignerSpec {
  commitment: string;
  scheme: SignatureScheme;
}

/**
 * An approver as a bare commitment, which takes the configuration's default
 * scheme, or as a {@link SignerSpec} carrying its own scheme.
 */
export type SignerInput = string | SignerSpec;

export interface MultisigConfig {
  threshold: number;
  /**
   * Approvers in storage order. A bare commitment uses `signatureScheme`
   * (default `'falcon'`); a {@link SignerSpec} sets that approver's scheme.
   */
  signerCommitments: SignerInput[];
  guardianCommitment: string;
  guardianPublicKey?: string;
  storageMode?: 'private' | 'public';
  procedureThresholds?: ProcedureThreshold[];
  /** The GUARDIAN's scheme, and the scheme of every approver given as a bare commitment. */
  signatureScheme?: SignatureScheme;
  seed?: Uint8Array
}

export interface CreateAccountResult {
  account: Account;
  seed: Uint8Array;
}

export type TransactionType =
  | { type: 'p2id'; recipient: string; faucetId: string; amount: bigint }
  | { type: 'consumeNotes'; noteIds: string[] }
  | { type: 'updateSigners'; newThreshold: number; newSignerCommitments: SignerInput[] }
  | { type: 'updateProcedureThreshold'; procedure: ProcedureName; threshold: number };

export interface NoteAsset {
  faucetId: string;
  amount: bigint;
}

export interface ConsumableNote {
  id: string;
  assets: NoteAsset[];
}
