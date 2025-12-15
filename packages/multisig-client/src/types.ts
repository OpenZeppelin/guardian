/**
 * Core types for the Miden multisig client.
 */

// =============================================================================
// Account Types
// =============================================================================

/**
 * Represents a multisig account state.
 */
export interface MultisigAccountState {
  /** Account ID as hex string (0x + 30 hex chars) */
  id: string;
  /** Current nonce for the account */
  nonce: number;
  /** Number of signatures required to execute a transaction */
  threshold: number;
  /** Public key commitments of all cosigners (0x + 64 hex chars each) */
  cosignerCommitments: string[];
}

/**
 * @deprecated Use MultisigAccountState instead
 */
export type MultisigAccount = MultisigAccountState;

// =============================================================================
// Account Creation Config
// =============================================================================

/**
 * Configuration for creating a multisig account.
 */
export interface MultisigConfig {
  /** Minimum number of signatures required to authorize a transaction */
  threshold: number;
  /** Public key commitments of all signers (hex strings, 64 chars each) */
  signerCommitments: string[];
  /** PSM server public key commitment (hex string) */
  psmCommitment: string;
  /** Whether PSM verification is enabled (default: true) */
  psmEnabled?: boolean;
}

/**
 * Result of account creation.
 */
export interface CreateAccountResult {
  /** The created account (from Miden SDK) */
  account: unknown;
  /** The account seed used for creation */
  seed: Uint8Array;
}

// =============================================================================
// Proposal Types
// =============================================================================

/**
 * Status of a proposal.
 */
export type ProposalStatus =
  | { type: 'pending'; signaturesCollected: number; signaturesRequired: number; signers: string[] }
  | { type: 'ready' }
  | { type: 'finalized' };

/**
 * A Falcon signature wrapper.
 */
export interface FalconSignature {
  Falcon: {
    signature: string;
  };
}

/**
 * A signature entry for a proposal.
 */
export interface ProposalSignatureEntry {
  signerId: string;
  signature: FalconSignature;
  timestamp: string;
}

/**
 * A transaction proposal.
 */
export interface Proposal {
  /** Unique proposal ID (commitment hash) */
  id: string;
  /** Account ID this proposal is for */
  accountId: string;
  /** Nonce for this proposal */
  nonce: number;
  /** Current status of the proposal */
  status: ProposalStatus;
  /** Serialized transaction summary (base64 encoded) */
  txSummary: string;
  /** Signatures collected so far */
  signatures: ProposalSignatureEntry[];
}

/**
 * Exported proposal for offline signing.
 */
export interface ExportedProposal {
  accountId: string;
  nonce: number;
  commitment: string;
  txSummaryBase64: string;
  signatures: Array<{
    commitment: string;
    signatureHex: string;
  }>;
}

// =============================================================================
// Transaction Types
// =============================================================================

/**
 * Types of transactions that can be proposed.
 */
export type TransactionType =
  | { type: 'p2id'; recipient: string; faucetId: string; amount: bigint }
  | { type: 'consumeNotes'; noteIds: string[] }
  | { type: 'updateSigners'; newThreshold: number; newSignerCommitments: string[] };

// =============================================================================
// PSM API Types (HTTP Interface)
// =============================================================================

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
 * Delta object from PSM API.
 */
export interface DeltaObject {
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
