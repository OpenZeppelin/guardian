/**
 * Types for the PSM (Private State Manager) client.
 */

// =============================================================================
// Signer Interface
// =============================================================================

/**
 * Interface for signing operations.
 * Implementations must provide Falcon signature generation.
 */
export interface Signer {
  /** The signer's public key commitment (0x + 64 hex chars) */
  readonly commitment: string;
  /** The signer's full public key as hex (used for x-pubkey header) */
  readonly publicKey: string;
  /** Signs an account ID and returns the signature hex */
  signAccountId(accountId: string): string;
  /** Signs a commitment/word and returns the signature hex */
  signCommitment(commitmentHex: string): string;
}

// =============================================================================
// PSM API Types (HTTP Interface)
// =============================================================================

/**
 * A Falcon signature wrapper.
 * Matches Rust: #[serde(tag = "scheme", rename_all = "snake_case")]
 */
export interface FalconSignature {
  scheme: 'falcon';
  signature: string;
}

/**
 * Authentication configuration for an account.
 */
export interface AuthConfig {
  MidenFalconRpo: {
    cosigner_commitments: string[];
  };
}

/**
 * Storage type for account data.
 */
export type StorageType = 'Filesystem';

/**
 * Cosigner signature in a delta status.
 */
export interface CosignerSignature {
  signer_id: string;
  signature: FalconSignature;
  timestamp: string;
}

/**
 * Delta status - matches server's tagged union.
 */
export type DeltaStatus =
  | { status: 'pending'; timestamp: string; proposer_id: string; cosigner_sigs: CosignerSignature[] }
  | { status: 'candidate'; timestamp: string }
  | { status: 'canonical'; timestamp: string }
  | { status: 'discarded'; timestamp: string };

/**
 * Proposal metadata - stored alongside the proposal to enable execution by any signer.
 * This metadata is needed to reconstruct the transaction for execution.
 */
export interface ProposalMetadata {
  /** Target threshold after the proposal is executed */
  targetThreshold: number;
  /** Target signer commitments after the proposal is executed */
  targetSignerCommitments: string[];
  /** Salt hex used in the transaction */
  saltHex: string;
}

/**
 * Delta object from PSM API (proposal format with tx_summary wrapper).
 */
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

/**
 * Delta object for execution (push_delta expects delta_payload to be just { data: string }).
 * This is the format the server's verify_delta/apply_delta expects.
 */
export interface ExecutionDelta {
  account_id: string;
  nonce: number;
  prev_commitment: string;
  new_commitment?: string;
  delta_payload: { data: string };
  ack_sig?: string;
  status: DeltaStatus;
}

/**
 * State object from PSM API.
 */
export interface StateObject {
  account_id: string;
  commitment: string;
  state_json: { data: string };
  created_at: string;
  updated_at: string;
}

// =============================================================================
// HTTP Request/Response Types
// =============================================================================

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
