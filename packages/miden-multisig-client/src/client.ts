/**
 * MultisigClient - Factory for creating and loading multisig accounts.
 *
 * This is the main entry point for the multisig SDK. It provides methods
 * to create new multisig accounts and load existing ones.
 */

import { type WebClient } from '@demox-labs/miden-sdk';
import { PsmHttpClient } from '@openzeppelin/psm-client';
import { Multisig } from './multisig.js';
import { createMultisigAccount } from './account/index.js';
import type { MultisigConfig, Signer } from './types.js';

/**
 * Configuration for MultisigClient.
 */
export interface MultisigClientConfig {
  /** PSM server endpoint (default: 'http://localhost:3000') */
  psmEndpoint?: string;
}

/**
 * Factory client for creating and loading multisig accounts.
 *
 * @example
 * ```typescript
 * import { MultisigClient, FalconSigner } from '@openzeppelin/miden-multisig-client';
 * import { WebClient, SecretKey } from '@demox-labs/miden-sdk';
 *
 * // Initialize
 * const webClient = await WebClient.createClient('https://rpc.testnet.miden.io:443');
 * const secretKey = SecretKey.rpoFalconWithRNG(seed);
 * const signer = new FalconSigner(secretKey);
 *
 * // Create client
 * const client = new MultisigClient(webClient, { psmEndpoint: 'http://localhost:3000' });
 *
 * // Get PSM pubkey for config
 * const psmCommitment = await client.psmClient.getPubkey();
 *
 * // Create multisig
 * const config = { threshold: 2, signerCommitments: [...], psmCommitment };
 * const multisig = await client.create(config, signer);
 * ```
 */
export class MultisigClient {
  private readonly webClient: WebClient;
  private readonly _psmClient: PsmHttpClient;

  constructor(webClient: WebClient, config: MultisigClientConfig = {}) {
    this.webClient = webClient;
    this._psmClient = new PsmHttpClient(config.psmEndpoint ?? 'http://localhost:3000');
  }

  /**
   * Access the internal PSM client.
   * Useful for getting the PSM pubkey before creating a multisig.
   */
  get psmClient(): PsmHttpClient {
    return this._psmClient;
  }

  /**
   * Create a new multisig account.
   *
   * @param config - Multisig configuration (threshold, signers, PSM commitment)
   * @param signer - The signer for this client (one of the cosigners)
   * @returns A Multisig instance wrapping the created account
   */
  async create(config: MultisigConfig, signer: Signer): Promise<Multisig> {
    // Set the signer on PSM client for authentication
    this._psmClient.setSigner(signer);

    // Create the multisig account using the Miden SDK
    const { account } = await createMultisigAccount(this.webClient, config);

    // Return wrapped Multisig instance
    return new Multisig(account, config, this._psmClient, signer);
  }

  /**
   * Load an existing multisig account from PSM.
   *
   * @param accountId - The account ID to load
   * @param config - Multisig configuration (must match the account)
   * @param signer - The signer for this client
   * @returns A Multisig instance for the loaded account
   */
  async load(accountId: string, config: MultisigConfig, signer: Signer): Promise<Multisig> {
    // Set the signer on PSM client for authentication
    this._psmClient.setSigner(signer);

    // Always pull from PSM (source of truth) and add to Miden SDK's local store
    const stateResponse = await this._psmClient.getState(accountId);

    // The state_json.data contains base64-encoded serialized account bytes
    const accountBase64 = stateResponse.state_json.data;
    if (!accountBase64) {
      throw new Error('No account data found in PSM state');
    }

    // Decode base64 to bytes
    const binaryString = atob(accountBase64);
    const accountBytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      accountBytes[i] = binaryString.charCodeAt(i);
    }

    // Deserialize the account using the SDK
    const { Account } = await import('@demox-labs/miden-sdk');
    const account = Account.deserialize(accountBytes);

    // Add to Miden SDK's local store (overwrite=true to handle reloads)
    await this.webClient.newAccount(account, true);

    // Return wrapped Multisig instance
    return new Multisig(null, config, this._psmClient, signer, accountId);
  }
}
