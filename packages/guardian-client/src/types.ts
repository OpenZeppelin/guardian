import type { RequestAuthPayload } from './auth-request.js';

export interface Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme;
  signAccountIdWithTimestamp(accountId: string, timestamp: number): Promise<string> | string;
  signRequest?(
    accountId: string,
    timestamp: number,
    requestPayload: RequestAuthPayload
  ): Promise<string> | string;
  signCommitment(commitmentHex: string): Promise<string> | string;

  /**
   * Sign the lookup-bound digest for `/state/lookup`. The implementation
   * MUST sign `LookupAuthMessage::to_word(timestamp_ms, key_commitment)` —
   * domain-separated from `AuthRequestMessage`. The canonical implementation
   * lives in `@openzeppelin/miden-multisig-client/lookupAuth.ts`; this
   * zero-dependency package does not pull in the Miden SDK to compute it.
   *
   * Optional: signers that do not implement it cause the HTTP client to
   * throw a clear error rather than send a signature the server will reject.
   */
  signLookupMessage?(
    keyCommitmentHex: string,
    timestampMs: number
  ): Promise<string> | string;
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

export type AuthConfig =
  | {
      MidenFalconRpo: {
        cosigner_commitments: string[];
      };
    }
  | {
      MidenEcdsa: {
        cosigner_commitments: string[];
      };
    };

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
  | 'update_procedure_threshold'
  | 'switch_guardian'
  | 'consume_notes'
  | 'p2id'
  | 'custom'
  | 'unknown';

export interface ProposalMetadata {
  proposalType?: ProposalType;
  targetThreshold?: number;
  requiredSignatures?: number;
  signerCommitments?: string[];
  targetProcedure?: string;
  salt?: string;
  description?: string;
  newGuardianPubkey?: string;
  newGuardianEndpoint?: string;
  noteIds?: string[];
  /** consume_notes metadata version (issue #229). Absent => v1. */
  consumeNotesMetadataVersion?: number;
  /** v2 embedded notes (base64), index-aligned with `noteIds`. */
  consumeNotesNotes?: string[];
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

export interface PubkeyResponse {
  commitment: string;
  pubkey?: string;
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

/**
 * Single match in a `/state/lookup` response. Wrapped in its own type so the
 * shape can be extended (role tags, per-account metadata) in a forward-
 * compatible way — additive server fields will not break existing clients.
 */
export interface LookupAccount {
  accountId: string;
}

/** Response shape for `lookupAccountByKeyCommitment`. */
export interface LookupResponse {
  accounts: LookupAccount[];
}
