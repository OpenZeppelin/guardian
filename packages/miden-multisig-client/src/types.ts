import type { Account } from '@demox-labs/miden-sdk';
import type { SignatureScheme } from '@openzeppelin/psm-client';
import type { ProcedureName } from './procedures.js';
import type { TransactionProposal } from './types/proposal.js';
import type { AccountState } from './multisig.js';
import type { DetectedMultisigConfig } from './inspector.js';

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
  TransactionProposal,
  TransactionProposalStatus,
  TransactionProposalSignature,
  ExportedTransactionProposal,
  SignTransactionProposalParams,
  ProposalMetadata,
  ProposalType,
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

export interface ProcedureThreshold {
  procedure: ProcedureName;
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
  | { type: 'updateSigners'; newThreshold: number; newSignerCommitments: string[] };

export interface NoteAsset {
  faucetId: string;
  amount: bigint;
}

export interface ConsumableNote {
  id: string;
  assets: NoteAsset[];
}
