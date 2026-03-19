/**
 * MultisigClient - Factory for creating and loading multisig accounts.
 *
 * This is the main entry point for the multisig SDK. It provides methods
 * to create new multisig accounts and load existing ones.
 */

import { type WebClient, Account, AccountId } from '@miden-sdk/miden-sdk';
import { GuardianHttpClient } from '@openzeppelin/guardian-client';
import { Multisig } from './multisig.js';
import { createMultisigAccount } from './account/index.js';
import { AccountInspector } from './inspector.js';
import type { MultisigConfig, Signer } from './types.js';

/**
 * Configuration for MultisigClient.
 */
export interface MultisigClientConfig {
  /** GUARDIAN server endpoint */
  guardianEndpoint?: string;
  /** Miden node RPC endpoint used for state commitment verification */
  midenRpcEndpoint?: string;
}

/**
 * Client for creating and loading multisig accounts.
 *
 * @example
 * ```typescript
 * import { MultisigClient, FalconSigner } from '@openzeppelin/miden-multisig-client';
 * import { WebClient, SecretKey } from '@miden-sdk/miden-sdk';
 *
 * // Initialize
 * const webClient = await WebClient.createClient('https://rpc.testnet.miden.io:443');
 * const secretKey = SecretKey.rpoFalconWithRNG(seed);
 * const signer = new FalconSigner(secretKey);
 *
 * // Create client
 * const client = new MultisigClient(webClient, {
 *   guardianEndpoint: 'http://localhost:3000',
 *   midenRpcEndpoint: 'https://rpc.testnet.miden.io:443',
 * });
 *
 * // Get GUARDIAN pubkey for config
 * const guardianCommitment = await client.guardianClient.getPubkey();
 *
 * // Create multisig
 * const config = { threshold: 2, signerCommitments: [...], guardianCommitment };
 * const multisig = await client.create(config, signer);
 * ```
 */
export class MultisigClient {
  private readonly webClient: WebClient;
  private readonly midenRpcEndpoint?: string;
  private _guardianClient: GuardianHttpClient;

  constructor(webClient: WebClient, config: MultisigClientConfig = {}) {
    this.webClient = webClient;
    this.midenRpcEndpoint = config.midenRpcEndpoint;
    this._guardianClient = new GuardianHttpClient(config.guardianEndpoint ?? 'http://localhost:3000');
  }

  /**
   * Change the GUARDIAN endpoint.
   * 
   * @param endpoint - The new GUARDIAN server endpoint URL
   */
  setGuardianEndpoint(endpoint: string): void {
    this._guardianClient = new GuardianHttpClient(endpoint);
  }

  /**
   * Access the internal GUARDIAN client.
   */
  get guardianClient(): GuardianHttpClient {
    return this._guardianClient;
  }

  /**
   * Create a new multisig account.
   *
   * @param config - Multisig configuration (threshold, signers, GUARDIAN commitment)
   * @param signer - The signer for this client (one of the cosigners)
   * @returns A Multisig instance wrapping the created account
   */
  async create(config: MultisigConfig, signer: Signer): Promise<Multisig> {
    this._guardianClient.setSigner(signer);

    const { account } = await createMultisigAccount(this.webClient, config);

    return new Multisig(
      account,
      config,
      this._guardianClient,
      signer,
      this.webClient,
      undefined,
      this.midenRpcEndpoint
    );
  }

  /**
   * Load an existing multisig account from GUARDIAN.
   *
   * @param accountId - The account ID to load
   * @param signer - The signer for this client
   * @returns A Multisig instance for the loaded account
   */
  async load(accountId: string, signer: Signer): Promise<Multisig> {
    this._guardianClient.setSigner(signer);

    const stateResponse = await this._guardianClient.getState(accountId);

    const accountBase64 = stateResponse.stateJson.data;
    if (!accountBase64) {
      throw new Error('No account data found in GUARDIAN state');
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
      guardianCommitment: detected.guardianCommitment ?? '',
      guardianEnabled: detected.guardianEnabled,
      procedureThresholds: Array.from(detected.procedureThresholds.entries()).map(
        ([procedure, threshold]) => ({ procedure, threshold })
      ),
    };

    const existingAccount = await this.webClient.getAccount(AccountId.fromHex(accountId));
    if (!existingAccount) {
        await this.webClient.newAccount(account, true);
    }

    return new Multisig(
      account,
      config,
      this._guardianClient,
      signer,
      this.webClient,
      accountId,
      this.midenRpcEndpoint
    );
  }
}
