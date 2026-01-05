/**
 * MultisigClient - Factory for creating and loading multisig accounts.
 *
 * This is the main entry point for the multisig SDK. It provides methods
 * to create new multisig accounts and load existing ones.
 */

import { type WebClient, Account } from '@demox-labs/miden-sdk';
import { PsmHttpClient } from '@openzeppelin/psm-client';
import { Multisig } from './multisig.js';
import { createMultisigAccount } from './account/index.js';
import { AccountInspector } from './inspector.js';
import type { MultisigConfig, Signer } from './types.js';

/**
 * Configuration for MultisigClient.
 */
export interface MultisigClientConfig {
  /** PSM server endpoint */
  psmEndpoint?: string;
}

/**
 * Client for creating and loading multisig accounts.
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
    this._psmClient.setSigner(signer);

    const { account } = await createMultisigAccount(this.webClient, config);

    return new Multisig(account, config, this._psmClient, signer);
  }

  /**
   * Load an existing multisig account from PSM.
   *
   * @param accountId - The account ID to load
   * @param signer - The signer for this client
   * @returns A Multisig instance for the loaded account
   */
  async load(accountId: string, signer: Signer): Promise<Multisig> {
    this._psmClient.setSigner(signer);

    const stateResponse = await this._psmClient.getState(accountId);

    const accountBase64 = stateResponse.stateJson.data;
    if (!accountBase64) {
      throw new Error('No account data found in PSM state');
    }

    const binaryString = atob(accountBase64);
    const accountBytes = new Uint8Array(binaryString.length);
    for (let i = 0; i < binaryString.length; i++) {
      accountBytes[i] = binaryString.charCodeAt(i);
    }
    const account = Account.deserialize(accountBytes);

    const detected = AccountInspector.fromAccount(account);
    const config: MultisigConfig = {
      threshold: detected.threshold,
      signerCommitments: detected.signerCommitments,
      psmCommitment: detected.psmCommitment ?? '',
      psmEnabled: detected.psmEnabled,
    };

    await this.webClient.newAccount(account, true);

    return new Multisig(null, config, this._psmClient, signer, accountId);
  }
}
