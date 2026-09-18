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
import { AccountInspector, assertCompleteDetectedConfig } from './inspector.js';
import { getRawMidenClient, requireConfigValue, requireMidenRpcEndpoint } from './raw-client.js';
import type { MultisigConfig, Signer } from './types.js';
import { isSafeToAdoptGuardianState, readOnChainCommitment } from './state/adopt.js';
import { normalizeHexWord } from './utils/encoding.js';
import {
  resolveProverConfig,
  type ProverConfig,
  type ResolvedProverConfig,
} from './prover/config.js';
import {
  resolveRpcConfig,
  type RpcConfig,
  type ResolvedRpcConfig,
} from './rpc/config.js';

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
  /** GUARDIAN server endpoint. Required — there is no default. */
  guardianEndpoint: string;
  /**
   * Miden node RPC endpoint used for proposal execution and state
   * commitment verification. Required — must point at the same network as
   * the injected `MidenClient`; there is no default.
   */
  midenRpcEndpoint: string;
  /** Multisig-owned remote prover override and proof retry policy. */
  prover?: ProverConfig;
  /** Retry policy for idempotent Miden node reads; submission is never retried. */
  rpc?: RpcConfig;
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
 *   prover: {
 *     url: 'https://prover.example',
 *     retry: { maxAttempts: 4 },
 *   },
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
  private readonly proverConfig: ResolvedProverConfig;
  private readonly rpcConfig: ResolvedRpcConfig;
  private _guardianClient: GuardianHttpClient;

  constructor(midenClient: MidenClient, config: MultisigClientConfig) {
    this.midenClient = midenClient;
    this.midenRpcEndpoint = requireMidenRpcEndpoint(config?.midenRpcEndpoint);
    this.proverConfig = resolveProverConfig(config?.prover, midenClient.defaultProver);
    this.rpcConfig = resolveRpcConfig(config?.rpc);
    this._guardianClient = new GuardianHttpClient(
      requireConfigValue('guardianEndpoint', config?.guardianEndpoint),
    );
  }

  /**
   * Change the GUARDIAN endpoint.
   *
   * @param endpoint - The new GUARDIAN server endpoint URL
   */
  setGuardianEndpoint(endpoint: string): void {
    this._guardianClient = new GuardianHttpClient(
      requireConfigValue('guardianEndpoint', endpoint),
    );
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
      this.midenRpcEndpoint,
      this.proverConfig,
      this.rpcConfig,
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
    const incomingAccount = Account.deserialize(accountBytes);

    const account = await this.adoptGuardianAccount(accountId, incomingAccount);

    const detected = AccountInspector.fromAccount(account);
    // Fail closed on a partial read: the detected signer set becomes the
    // authoritative config that membership proposals rewrite on-chain.
    assertCompleteDetectedConfig(detected);
    const config: MultisigConfig = {
      threshold: detected.threshold,
      signerCommitments: detected.signerCommitments,
      guardianCommitment: detected.guardianCommitment,
      procedureThresholds: Array.from(detected.procedureThresholds.entries()).map(
        ([procedure, threshold]) => ({ procedure, threshold })
      ),
    };

    await bindSignerAccountKey(signer, this.midenClient, accountId);

    return new Multisig(
      account,
      config,
      this._guardianClient,
      signer,
      this.midenClient,
      accountId,
      this.midenRpcEndpoint,
      this.proverConfig,
      this.rpcConfig,
    );
  }

  /**
   * Reconcile the account GUARDIAN returned with the local store, and answer
   * with the one that is actually current.
   *
   * Writing GUARDIAN's account only when the store held nothing meant a caller
   * that already had the account kept its own copy while the returned
   * `Multisig` carried a config derived from GUARDIAN's. One object, two
   * sources, silently disagreeing: after a membership change the config said
   * one thing and every account read said another, with no error.
   *
   * Overwriting unconditionally is not the fix either. Between pushing a delta
   * and GUARDIAN canonicalizing it, local is legitimately ahead, and clobbering
   * it would build the next transaction on a stale nonce. So this applies the
   * same rule `syncState` does, and returns whichever account wins so the
   * caller describes the state it will actually read.
   */
  private async adoptGuardianAccount(
    accountId: string,
    incomingAccount: Account,
  ): Promise<Account> {
    const localAccount = await this.midenClient.accounts.get(AccountId.fromHex(accountId));
    if (!localAccount) {
      // Still checked against chain, and through the same rule the other path
      // uses. An empty store is the ordinary shape for loading an account this
      // client has never held, so inserting without the check would make it the
      // one path where GUARDIAN's word is taken on its own. Checking only that
      // *some* commitment exists is not the check: it admits state that
      // disagrees with chain, which is the thing being guarded against.
      // Only for an account that has transacted. One that has not is not
      // deployed, so there is no on-chain commitment to agree or disagree with,
      // and reading the node for one would make loading a fresh account depend
      // on the node as well as on GUARDIAN.
      const transacted = incomingAccount.nonce().asInt() > BigInt(0);
      const onChain = transacted
        ? await readOnChainCommitment(
            this.midenRpcEndpoint,
            AccountId.fromHex(accountId),
            this.rpcConfig,
          )
        : null;
      // The one rule the shared predicate cannot apply here. It keys its
      // null-commitment guard on the *local* nonce, because `syncState`
      // legitimately meets a higher incoming nonce with no local history. With
      // no local record at all, the incoming nonce is the only evidence there
      // is: an account GUARDIAN reports as having transacted is deployed, so a
      // node reporting nothing for it is an RPC or gateway failure rather than
      // an undeployed account. `readOnChainCommitment` reports a missing
      // account by matching `not found`, which a 404 also says.
      if (transacted && !onChain) {
        throw new Error(
          `Refusing to adopt GUARDIAN state for ${accountId}: it has transacted (nonce ${incomingAccount.nonce().asInt().toString()}) but the node reported no on-chain commitment, so it could not be checked against chain`
        );
      }
      // Handed the commitment already read, rather than letting the predicate
      // read it again: two reads can disagree, and the guard above would then
      // have passed on a different answer than the comparison rejects.
      await isSafeToAdoptGuardianState({
        accountId,
        incomingAccount,
        readCommitment: () => Promise.resolve(onChain),
      });
      await this.midenClient.accounts.insert({ account: incomingAccount, overwrite: true });
      return incomingAccount;
    }

    const localCommitment = normalizeHexWord(localAccount.to_commitment().toHex());
    const incomingCommitment = normalizeHexWord(incomingAccount.to_commitment().toHex());
    // Equal commitments are the same state, and the safety rule reads an equal
    // nonce as divergence, so it must not be asked about a pair that agrees.
    if (localCommitment === incomingCommitment) {
      return localAccount;
    }

    const adopt = await isSafeToAdoptGuardianState({
      accountId,
      incomingAccount,
      localAccount,
      readCommitment: () =>
        readOnChainCommitment(this.midenRpcEndpoint, AccountId.fromHex(accountId), this.rpcConfig),
    });
    if (!adopt) {
      return localAccount;
    }

    await this.midenClient.accounts.insert({ account: incomingAccount, overwrite: true });
    return incomingAccount;
  }
}
