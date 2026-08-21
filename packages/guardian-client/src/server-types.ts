export interface ServerFalconSignature {
  scheme: 'falcon';
  signature: string;
}

export interface ServerEcdsaSignature {
  scheme: 'ecdsa';
  signature: string;
  public_key?: string;
}

export type ServerProposalSignature = ServerFalconSignature | ServerEcdsaSignature;

export interface ServerCosignerSignature {
  signer_id: string;
  signature: ServerProposalSignature;
  timestamp: string;
}

export type ServerDeltaStatus =
  | { status: 'pending'; timestamp: string; proposer_id: string; cosigner_sigs: ServerCosignerSignature[] }
  | { status: 'candidate'; timestamp: string }
  | { status: 'canonical'; timestamp: string }
  | { status: 'retained'; timestamp: string; reason?: 'retry_exhausted' | 'diverged' }
  | { status: 'discarded'; timestamp: string; reason?: string };

export type ServerProposalType =
  | 'add_signer'
  | 'remove_signer'
  | 'change_threshold'
  | 'update_procedure_threshold'
  | 'switch_guardian'
  | 'consume_notes'
  | 'p2id'
  | 'custom'
  // The server accepts arbitrary proposal types (issue #266); known literals are
  // kept for autocomplete while `(string & {})` admits any custom label.
  | (string & {});

export interface ServerProposalMetadata {
  proposal_type?: ServerProposalType;
  target_threshold?: number;
  required_signatures?: number;
  signer_commitments?: string[];
  target_procedure?: string;
  salt?: string;
  description?: string;
  new_guardian_pubkey?: string;
  new_guardian_endpoint?: string;
  note_ids?: string[];
  /** consume_notes metadata version (issue #229). Absent => v1. */
  consume_notes_metadata_version?: number;
  /** v2 embedded notes (base64), index-aligned with `note_ids`. */
  consume_notes_notes?: string[];
  recipient_id?: string;
  faucet_id?: string;
  amount?: string;
  /** P2ID note visibility, "public" or "private" (issue #322). Absent => public. */
  note_type?: string;
  /** P2IDE reclaim block height (issue #366). Presence of either height means a P2IDE note. */
  reclaim_height?: number;
  /** P2IDE timelock block height (issue #366). */
  timelock_height?: number;
}

export interface ServerDeltaObject {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: {
    tx_summary?: { data: string };
    data?: string;
    signatures?: Array<{ signer_id: string; signature: ServerProposalSignature }>;
    metadata?: ServerProposalMetadata;
  };
  ack_sig?: string;
  ack_pubkey?: string;
  ack_scheme?: string;
  status: ServerDeltaStatus;
}

export interface ServerPushDeltaResponse {
  account_id: string;
  nonce: number;
  new_commitment?: string;
  ack_sig?: string;
  ack_pubkey?: string;
  ack_scheme?: string;
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
  auth_scheme?: string;
}

export type ServerAuthConfig =
  | { MidenFalconRpo: { cosigner_commitments: string[] } }
  | { MidenEcdsa: { cosigner_commitments: string[] } };

export interface ServerConfigureRequest {
  account_id: string;
  auth: ServerAuthConfig;
  initial_state: { data: string; account_id: string };
}

export interface ServerConfigureResponse {
  success: boolean;
  message: string;
  ack_pubkey?: string;
  ack_commitment?: string;
}

export interface ServerDeltaProposalRequest {
  account_id: string;
  nonce: number;
  delta_payload: {
    tx_summary: { data: string };
    signatures: Array<{ signer_id: string; signature: ServerProposalSignature }>;
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

export interface ServerAbandonCandidateRequest {
  account_id: string;
  nonce: number;
}

export interface ServerAbandonCandidateResponse {
  account_id: string;
  nonce: number;
  state: 'pending' | 'abandoned' | 'retained';
  abandon_requested_at?: string;
}

export interface ServerSignProposalRequest {
  account_id: string;
  commitment: string;
  signature: ServerProposalSignature;
}

export interface ServerPubkeyResponse {
  commitment: string;
  pubkey?: string;
}

export interface ServerStatusResponse {
  status: string;
  version: string;
  git_commit: string;
  environment: string;
  started_at: string;
  uptime_seconds: number;
}

export interface ServerLookupAccount {
  account_id: string;
}

export interface ServerLookupResponse {
  accounts: ServerLookupAccount[];
}

// --- Delta history (issue #413) ---

export interface ServerHistoryNoteAsset {
  asset_id: string;
  kind: 'fungible' | 'non_fungible';
  amount?: string;
}

export interface ServerHistoryNote {
  note_id: string;
  tag: 'p2id' | 'p2ide' | 'pswap' | 'mint' | 'burn' | 'custom';
  note_type: 'public' | 'private';
  assets: ServerHistoryNoteAsset[];
  sender?: string;
  recipient?: string;
}

export interface ServerHistoryDecodeWarning {
  section: 'tx_summary' | 'metadata' | 'input_notes' | 'output_notes' | 'vault' | 'storage';
  reason: string;
}

export interface ServerHistoryEntry {
  nonce: number;
  status: 'canonical';
  timestamp: string;
  new_commitment: string | null;
  input_notes: ServerHistoryNote[];
  output_notes: ServerHistoryNote[];
  decode_warnings?: ServerHistoryDecodeWarning[];
}

export interface ServerHistoryPage {
  items: ServerHistoryEntry[];
  next_cursor: string | null;
}
