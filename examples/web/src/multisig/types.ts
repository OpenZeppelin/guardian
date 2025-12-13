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
  /** The created account */
  account: import('@demox-labs/miden-sdk').Account;
  /** The account seed used for creation */
  seed: Uint8Array;
}
