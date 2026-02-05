export interface Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme;
  signAccountIdWithTimestamp(accountId: string, timestamp: number): string;
  signCommitment(commitmentHex: string): string;
}

export interface FalconSignature {
  scheme: 'falcon';
  signature: string;
}

export interface EcdsaSignature {
  scheme: 'ecdsa';
  signature: string;
  publicKey?: string;
}

export type ProposalSignature = FalconSignature | EcdsaSignature;

export type SignatureScheme = 'falcon' | 'ecdsa';

export interface PubkeyResponse {
  commitment: string;
  pubkey?: string;
}

export type AuthConfig =
  | { MidenFalconRpo: { cosigner_commitments: string[] } }
  | { MidenEcdsa: { cosigner_commitments: string[] } };


export interface CosignerSignature {
  signerId: string;
  signature: ProposalSignature;
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
  | 'custom'
  | 'unknown';

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
    signatures: Array<{ signerId: string; signature: ProposalSignature }>;
    metadata?: ProposalMetadata;
  };
  ackSig?: string;
  ackPubkey?: string;
  ackScheme?: string;
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
  authScheme?: string;
}

export interface ConfigureRequest {
  accountId: string;
  auth: AuthConfig;
  initialState: { data: string; accountId: string };
}

export interface ConfigureResponse {
  success: boolean;
  message: string;
  ackPubkey?: string;
  ackCommitment?: string;
}

export interface DeltaProposalRequest {
  accountId: string;
  nonce: number;
  deltaPayload: {
    txSummary: { data: string };
    signatures: Array<{ signerId: string; signature: ProposalSignature }>;
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
  signature: ProposalSignature;
}

export interface PushDeltaResponse {
  accountId: string;
  nonce: number;
  newCommitment?: string;
  ackSig?: string;
  ackPubkey?: string;
  ackScheme?: string;
}
