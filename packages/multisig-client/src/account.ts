/**
 * MultisigAccount wrapper class.
 *
 * Provides a typed wrapper around Miden accounts with multisig + PSM configuration.
 * Mirrors the Rust MultisigAccount struct from miden-multisig-client.
 */

// =============================================================================
// MultisigAccount Class
// =============================================================================

/**
 * Wrapper around a Miden account with multisig + PSM configuration.
 *
 * Provides typed accessors for storage slots following the layout:
 *
 * **Multisig Component (4 slots):**
 * - Slot 0: Threshold config `[threshold, num_signers, 0, 0]`
 * - Slot 1: Signer public keys map `[index, 0, 0, 0] => COMMITMENT`
 * - Slot 2: Executed transactions map (for replay protection)
 * - Slot 3: Procedure threshold overrides map
 *
 * **PSM Component (2 slots, offset by 4):**
 * - Slot 4: PSM selector `[1, 0, 0, 0]` (ON) or `[0, 0, 0, 0]` (OFF)
 * - Slot 5: PSM public key map `[0, 0, 0, 0] => PSM_COMMITMENT`
 */
export class MultisigAccount {
  /**
   * The underlying Miden account (from SDK).
   */
  readonly account: unknown;

  /**
   * The account ID as hex string.
   */
  readonly id: string;

  /**
   * The PSM server endpoint for this account.
   */
  readonly psmEndpoint: string;

  /**
   * Configuration values extracted from storage.
   */
  private readonly _threshold: number;
  private readonly _numSigners: number;
  private readonly _cosignerCommitments: string[];
  private readonly _psmEnabled: boolean;
  private readonly _psmCommitment: string;

  /**
   * Creates a MultisigAccount wrapper.
   *
   * @param account - The underlying Miden account
   * @param id - The account ID
   * @param psmEndpoint - The PSM server endpoint
   * @param config - Pre-extracted configuration
   */
  constructor(
    account: unknown,
    id: string,
    psmEndpoint: string,
    config: {
      threshold: number;
      numSigners: number;
      cosignerCommitments: string[];
      psmEnabled: boolean;
      psmCommitment: string;
    }
  ) {
    this.account = account;
    this.id = id;
    this.psmEndpoint = psmEndpoint;
    this._threshold = config.threshold;
    this._numSigners = config.numSigners;
    this._cosignerCommitments = config.cosignerCommitments;
    this._psmEnabled = config.psmEnabled;
    this._psmCommitment = config.psmCommitment;
  }

  /**
   * The number of signatures required to authorize a transaction.
   */
  get threshold(): number {
    return this._threshold;
  }

  /**
   * The number of cosigners.
   */
  get numSigners(): number {
    return this._numSigners;
  }

  /**
   * The public key commitments of all cosigners.
   */
  get cosignerCommitments(): readonly string[] {
    return this._cosignerCommitments;
  }

  /**
   * Whether PSM verification is enabled.
   */
  get psmEnabled(): boolean {
    return this._psmEnabled;
  }

  /**
   * The PSM server's public key commitment.
   */
  get psmCommitment(): string {
    return this._psmCommitment;
  }

  /**
   * Checks if a commitment belongs to a cosigner of this account.
   *
   * @param commitment - The commitment to check (hex string)
   * @returns True if the commitment is a cosigner
   */
  isCosigner(commitment: string): boolean {
    const normalizedCommitment = commitment.toLowerCase().replace(/^0x/, '');
    return this._cosignerCommitments.some(
      (c) => c.toLowerCase().replace(/^0x/, '') === normalizedCommitment
    );
  }

  /**
   * Creates a MultisigAccount from a newly created account and config.
   *
   * @param account - The created Miden account
   * @param id - The account ID
   * @param psmEndpoint - The PSM server endpoint
   * @param config - The multisig configuration used to create the account
   */
  static fromConfig(
    account: unknown,
    id: string,
    psmEndpoint: string,
    config: {
      threshold: number;
      signerCommitments: string[];
      psmCommitment: string;
      psmEnabled?: boolean;
    }
  ): MultisigAccount {
    return new MultisigAccount(account, id, psmEndpoint, {
      threshold: config.threshold,
      numSigners: config.signerCommitments.length,
      cosignerCommitments: [...config.signerCommitments],
      psmEnabled: config.psmEnabled !== false,
      psmCommitment: config.psmCommitment,
    });
  }

  /**
   * Serializes the account configuration to JSON.
   */
  toJSON(): object {
    return {
      id: this.id,
      psmEndpoint: this.psmEndpoint,
      threshold: this._threshold,
      numSigners: this._numSigners,
      cosignerCommitments: this._cosignerCommitments,
      psmEnabled: this._psmEnabled,
      psmCommitment: this._psmCommitment,
    };
  }
}
