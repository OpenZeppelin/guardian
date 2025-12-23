import type { Account } from '@demox-labs/miden-sdk';
import type {
  ExportedProposal,
  Proposal,
  ProposalMetadata,
  ProposalSignatureEntry,
  ProposalStatus,
} from './types/proposal.js';

export type {
  Signer,
  FalconSignature,
  CosignerSignature,
  AuthConfig,
  StorageType,
  DeltaStatus,
  DeltaObject,
  StateObject,
  ProposalType,
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
} from './types/proposal.js';

export interface MultisigAccountState {
  id: string;
  nonce: number;
  threshold: number;
  cosignerCommitments: string[];
}

export interface MultisigConfig {
  threshold: number;
  signerCommitments: string[];
  psmCommitment: string;
  psmEnabled?: boolean;
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
