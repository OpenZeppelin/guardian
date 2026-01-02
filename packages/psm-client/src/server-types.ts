// Internal server format types (snake_case matching server API).
// These are used internally for serialization/deserialization.
// Public API uses camelCase types from types.ts.

export interface ServerFalconSignature {
  scheme: 'falcon';
  signature: string;
}

export interface ServerCosignerSignature {
  signer_id: string;
  signature: ServerFalconSignature;
  timestamp: string;
}

export type ServerDeltaStatus =
  | { status: 'pending'; timestamp: string; proposer_id: string; cosigner_sigs: ServerCosignerSignature[] }
  | { status: 'candidate'; timestamp: string }
  | { status: 'canonical'; timestamp: string }
  | { status: 'discarded'; timestamp: string };

export type ServerProposalType =
  | 'add_signer'
  | 'remove_signer'
  | 'change_threshold'
  | 'switch_psm'
  | 'consume_notes'
  | 'p2id'
  | 'custom';

export interface ServerProposalMetadata {
  proposal_type?: ServerProposalType;
  target_threshold?: number;
  signer_commitments?: string[];
  salt?: string;
  description?: string;
  new_psm_pubkey?: string;
  new_psm_endpoint?: string;
  note_ids?: string[];
  recipient_id?: string;
  faucet_id?: string;
  amount?: string;
}

export interface ServerDeltaObject {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: {
    tx_summary?: { data: string };
    data?: string;
    signatures?: Array<{ signer_id: string; signature: ServerFalconSignature }>;
    metadata?: ServerProposalMetadata;
  };
  ack_sig?: string;
  status: ServerDeltaStatus;
}

export interface ServerPushDeltaResponse {
  account_id: string;
  nonce: number;
  new_commitment?: string;
  ack_sig?: string;
}

export interface ServerExecutionDelta {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: { data: string };
  ack_sig?: string;
  status: ServerDeltaStatus;
}

export interface ServerStateObject {
  account_id: string;
  commitment: string;
  state_json: { data: string };
  created_at: string;
  updated_at: string;
}

export interface ServerAuthConfig {
  MidenFalconRpo: {
    cosigner_commitments: string[];
  };
}

export type ServerStorageType = 'Filesystem';

export interface ServerConfigureRequest {
  account_id: string;
  auth: ServerAuthConfig;
  initial_state: { data: string; account_id: string };
  storage_type: ServerStorageType;
}

export interface ServerConfigureResponse {
  success: boolean;
  message: string;
  ack_pubkey?: string;
}

export interface ServerDeltaProposalRequest {
  account_id: string;
  nonce: number;
  delta_payload: {
    tx_summary: { data: string };
    signatures: Array<{ signer_id: string; signature: ServerFalconSignature }>;
    metadata?: ServerProposalMetadata;
  };
}

export interface ServerDeltaProposalResponse {
  delta: ServerDeltaObject;
  commitment: string;
}

export interface ServerProposalsResponse {
  proposals: ServerDeltaObject[];
}

export interface ServerSignProposalRequest {
  account_id: string;
  commitment: string;
  signature: ServerFalconSignature;
}

export interface ServerPubkeyResponse {
  pubkey: string;
}
