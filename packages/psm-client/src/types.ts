
export interface Signer {
  readonly commitment: string;
  readonly publicKey: string;
  signAccountId(accountId: string): string;
  signCommitment(commitmentHex: string): string;
}

export interface FalconSignature {
  scheme: 'falcon';
  signature: string;
}

export interface AuthConfig {
  MidenFalconRpo: {
    cosigner_commitments: string[];
  };
}

export type StorageType = 'Filesystem';

export interface CosignerSignature {
  signerId: string;
  signature: FalconSignature;
  timestamp: string;
}

export type DeltaStatus =
  | { status: 'pending'; timestamp: string; proposerId: string; cosignerSigs: CosignerSignature[] }
  | { status: 'candidate'; timestamp: string }
  | { status: 'canonical'; timestamp: string }
  | { status: 'discarded'; timestamp: string };

export type ProposalType =
  | 'add_signer'
  | 'remove_signer'
  | 'change_threshold'
  | 'switch_psm'
  | 'consume_notes'
  | 'p2id'
  | 'custom';

export interface ProposalMetadata {
  proposalType?: ProposalType;
  targetThreshold?: number;
  signerCommitments?: string[];
  salt?: string;
  description?: string;
  newPsmPubkey?: string;
  newPsmEndpoint?: string;
  noteIds?: string[];
  recipientId?: string;
  faucetId?: string;
  amount?: string;
}

export interface DeltaObject {
  accountId: string;
  nonce: number;
  prevCommitment: string;
  newCommitment?: string;
  deltaPayload: {
    txSummary: { data: string };
    signatures: Array<{ signerId: string; signature: FalconSignature }>;
    metadata?: ProposalMetadata;
  };
  ackSig?: string;
  status: DeltaStatus;
}

export interface ExecutionDelta {
  accountId: string;
  nonce: number;
  prevCommitment: string;
  newCommitment?: string;
  deltaPayload: { data: string };
  ackSig?: string;
  status: DeltaStatus;
}

export interface StateObject {
  accountId: string;
  commitment: string;
  stateJson: { data: string };
  createdAt: string;
  updatedAt: string;
}

export interface ConfigureRequest {
  accountId: string;
  auth: AuthConfig;
  initialState: { data: string; accountId: string };
  storageType: StorageType;
}

export interface ConfigureResponse {
  success: boolean;
  message: string;
  ackPubkey?: string;
}

export interface PubkeyResponse {
  pubkey: string;
}

export interface DeltaProposalRequest {
  accountId: string;
  nonce: number;
  deltaPayload: {
    txSummary: { data: string };
    signatures: Array<{ signerId: string; signature: FalconSignature }>;
    metadata?: ProposalMetadata;
  };
}

export interface DeltaProposalResponse {
  delta: DeltaObject;
  commitment: string;
}

export interface ProposalsResponse {
  proposals: DeltaObject[];
}

export interface SignProposalRequest {
  accountId: string;
  commitment: string;
  signature: FalconSignature;
}

export interface PushDeltaResponse {
  accountId: string;
  nonce: number;
  newCommitment?: string;
  ackSig?: string;
}
