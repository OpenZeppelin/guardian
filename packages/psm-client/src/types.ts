// Types for the PSM (Private State Manager) client.

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
  signer_id: string;
  signature: FalconSignature;
  timestamp: string;
}

export type DeltaStatus =
  | { status: 'pending'; timestamp: string; proposer_id: string; cosigner_sigs: CosignerSignature[] }
  | { status: 'candidate'; timestamp: string }
  | { status: 'canonical'; timestamp: string }
  | { status: 'discarded'; timestamp: string };

export type ProposalType = 'add_signer' | 'remove_signer' | 'change_threshold' | 'switch_psm' | 'consume_notes' | 'p2id' | 'custom';

export interface ProposalMetadata {
  proposalType?: ProposalType;
  targetThreshold?: number;
  targetSignerCommitments?: string[];
  saltHex?: string;
  description?: string;
  newPsmPubkey?: string;
  newPsmEndpoint?: string;
  noteIds?: string[];
  recipientId?: string;
  faucetId?: string;
  amount?: string;
}

export interface DeltaObject {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: {
    tx_summary: { data: string };
    signatures: Array<{ signer_id: string; signature: FalconSignature }>;
    metadata?: ProposalMetadata;
  };
  ack_sig?: string;
  status: DeltaStatus;
}

export interface ExecutionDelta {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: { data: string };
  ack_sig?: string;
  status: DeltaStatus;
}

export interface StateObject {
  account_id: string;
  commitment: string;
  state_json: { data: string };
  created_at: string;
  updated_at: string;
}

export interface ConfigureRequest {
  account_id: string;
  auth: AuthConfig;
  initial_state: { data: string; account_id: string };
  storage_type: StorageType;
}

export interface ConfigureResponse {
  success: boolean;
  message: string;
  ack_pubkey?: string;
}

export interface PubkeyResponse {
  pubkey: string;
}

export interface DeltaProposalRequest {
  account_id: string;
  nonce: number;
  delta_payload: {
    tx_summary: { data: string };
    signatures: Array<{ signer_id: string; signature: FalconSignature }>;
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
  account_id: string;
  commitment: string;
  signature: FalconSignature;
}
