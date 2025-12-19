/**
 * Core types for the Miden multisig client.
 */

import type { Account } from '@demox-labs/miden-sdk';
import type { ProposalType as PsmProposalType } from '@openzeppelin/psm-client';

// Re-export PSM types that are used in multisig context
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

// Local alias for use in this file
type ProposalType = PsmProposalType;

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
  account: Account;
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
 * A signature entry for a proposal.
 */
export interface ProposalSignatureEntry {
  signerId: string;
  signature: { scheme: 'falcon'; signature: string };
  timestamp: string;
}

/**
 * Metadata needed to reconstruct and finalize a proposal.
 */
export interface ProposalMetadata {
  /** Type of proposal for UI display */
  proposalType?: ProposalType;
  /** Target threshold (for signer update proposals) */
  targetThreshold?: number;
  /** Target signer commitments (for signer update proposals) */
  targetSignerCommitments?: string[];
  /** Salt used for transaction authentication (hex) */
  saltHex?: string;
  /** Human-readable description of the change */
  description?: string;
  /** New PSM public key commitment (for switch_psm proposals) */
  newPsmPubkey?: string;
  /** New PSM endpoint URL (for switch_psm proposals) */
  newPsmEndpoint?: string;
  /** Note IDs to consume (for consume_notes proposals) */
  noteIds?: string[];
  /** Recipient account ID for P2ID (hex) */
  recipientId?: string;
  /** Faucet/token ID for P2ID (hex) */
  faucetId?: string;
  /** Amount to send for P2ID (string to handle bigint serialization) */
  amount?: string;
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
  /** Metadata needed for execution (target config, salt, etc.) */
  metadata?: ProposalMetadata;
}

/**
 * Exported proposal for offline signing and side-channel sharing.
 */
export interface ExportedProposal {
  accountId: string;
  nonce: number;
  commitment: string;
  txSummaryBase64: string;
  signatures: Array<{
    commitment: string;
    signatureHex: string;
    timestamp?: string;
  }>;
  /** Metadata needed for execution (required for import/export flow) */
  metadata?: ProposalMetadata;
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
// Note Types
// =============================================================================

/**
 * Represents an asset in a note.
 */
export interface NoteAsset {
  /** Faucet account ID (hex string) */
  faucetId: string;
  /** Amount of the fungible asset */
  amount: bigint;
}

/**
 * A note that can be consumed by the multisig account.
 */
export interface ConsumableNote {
  /** Note ID (hex string) */
  id: string;
  /** Assets contained in the note */
  assets: NoteAsset[];
}
