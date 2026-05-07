/**
 * MultisigClient - Factory for creating and loading multisig accounts.
 *
 * This is the main entry point for the multisig SDK. It provides methods
 * to create new multisig accounts and load existing ones.
 */

import { type MidenClient, Account, AccountId } from '@miden-sdk/miden-sdk';
import { GuardianHttpClient } from '@openzeppelin/guardian-client';
import type { StateObject } from '@openzeppelin/guardian-client';
import { Multisig } from './multisig.js';
import { createMultisigAccount } from './account/index.js';
import { AccountInspector } from './inspector.js';
import { getRawMidenClient, resolveMidenRpcEndpoint } from './raw-client.js';
import type { MultisigConfig, Signer } from './types.js';

interface AccountKeyBindingSigner {
  bindAccountKey?(midenClient: MidenClient, accountId: string): Promise<void>;
}

async function bindSignerAccountKey(
  signer: Signer,
  midenClient: MidenClient,
  accountId: string,
): Promise<void> {
  const bindingSigner = signer as Signer & AccountKeyBindingSigner;
  if (typeof bindingSigner.bindAccountKey === 'function') {
    await bindingSigner.bindAccountKey(midenClient, accountId);
  }
}

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
 * One match returned by `MultisigClient.recoverByKey`. Pairs the discovered
 * `accountId` with the current `state` snapshot so callers do not need to do a
 * second round-trip per account.
 */
export interface RecoveredAccount {
  accountId: string;
  state: StateObject;
}

/**
 * Client for creating and loading multisig accounts.
 *
 * @example
 * ```typescript
 * import { MultisigClient, FalconSigner } from '@openzeppelin/miden-multisig-client';
 * import { MidenClient, AuthSecretKey } from '@miden-sdk/miden-sdk';
 *
 * // Initialize
 * const midenClient = await MidenClient.createDevnet();
 * const secretKey = AuthSecretKey.rpoFalconWithRNG(seed);
 * const signer = new FalconSigner(secretKey);
 *
 * // Create client
 * const client = new MultisigClient(midenClient, {
 *   guardianEndpoint: 'http://localhost:3000',
 *   midenRpcEndpoint: 'https://rpc.devnet.miden.io',
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
  private readonly midenClient: MidenClient;
  private readonly midenRpcEndpoint: string;
  private _guardianClient: GuardianHttpClient;

  constructor(midenClient: MidenClient, config: MultisigClientConfig = {}) {
    this.midenClient = midenClient;
    this.midenRpcEndpoint = resolveMidenRpcEndpoint(config.midenRpcEndpoint);
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
   * Recover the set of accounts a given signer authorizes by querying
   * Guardian's `/state/lookup` endpoint and fetching state for each match.
   * Returns `(accountId, state)` pairs; an empty array means no account on
   * this operator authorizes the commitment (distinct from "wrong key",
   * which would fail authentication first).
   *
   * @throws if `signer` does not implement `signLookupMessage`. The bundled
   *   `FalconSigner` and `EcdsaSigner` both do.
   */
  async recoverByKey(signer: Signer): Promise<RecoveredAccount[]> {
    this._guardianClient.setSigner(signer);

    const lookup = await this._guardianClient.lookupAccountByKeyCommitment(signer.commitment);

    // Per-account replay protection is scoped to each account's
    // last_auth_timestamp, so concurrent getState calls do not race.
    return Promise.all(
      lookup.accounts.map(async ({ accountId }) => ({
        accountId,
        state: await this._guardianClient.getState(accountId),
      })),
    );
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

    const { account } = await createMultisigAccount(
      this.midenClient,
      config,
      this.midenRpcEndpoint,
    );
    const accountId = account.id().toString();
    await bindSignerAccountKey(signer, this.midenClient, accountId);

    return new Multisig(
      account,
      config,
      this._guardianClient,
      signer,
      this.midenClient,
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

    const existingAccount = await this.midenClient.accounts.get(AccountId.fromHex(accountId));
    if (!existingAccount) {
      await this.midenClient.accounts.insert({ account, overwrite: true });
    }
    await bindSignerAccountKey(signer, this.midenClient, accountId);

    return new Multisig(
      account,
      config,
      this._guardianClient,
      signer,
      this.midenClient,
      accountId,
      this.midenRpcEndpoint
    );
  }
}
