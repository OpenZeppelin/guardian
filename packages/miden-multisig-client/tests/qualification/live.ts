import { AuthSecretKey, MidenClient } from '@miden-sdk/miden-sdk';

import { MultisigClient } from '../../src/client.js';
import type { Multisig } from '../../src/multisig.js';
import { EcdsaSigner } from '../../src/signers/ecdsa.js';
import { FalconSigner } from '../../src/signers/falcon.js';
import type { Signer } from '../../src/types.js';

export type Scheme = 'falcon' | 'ecdsa';
export type NetworkName = 'devnet' | 'testnet';

export interface LiveConfig {
  readonly network: NetworkName;
  readonly guardianEndpoint: string;
  readonly midenRpcEndpoint: string;
  /** A second GUARDIAN deployment, required only by the migration scenario. */
  readonly migrationEndpoint?: string;
}

/**
 * One cosigner: its own signer, its own Miden client, and its own store.
 *
 * Cosigners are separate parties. Sharing a store between them would let one
 * see another's local state, which is what a threshold exists to prevent, and
 * would make a 2-of-3 pass for reasons no real deployment enjoys.
 */
export interface Cosigner {
  readonly signer: Signer;
  /**
   * Kept alongside the signer so a cosigner's key can be handed to the other
   * SDK. The signer itself does not expose it, and it should not: only this
   * harness has a reason to move key material between processes.
   */
  readonly secretKey: AuthSecretKey;
  readonly midenClient: MidenClient;
  readonly multisigClient: MultisigClient;
}

function makeSigner(scheme: Scheme): { signer: Signer; secretKey: AuthSecretKey } {
  const secretKey =
    scheme === 'falcon'
      ? AuthSecretKey.rpoFalconWithRNG(undefined)
      : AuthSecretKey.ecdsaWithRNG(undefined);
  const signer = scheme === 'falcon' ? new FalconSigner(secretKey) : new EcdsaSigner(secretKey);
  return { signer, secretKey };
}

export async function buildCosigners(
  config: LiveConfig,
  count: number,
  scheme: Scheme,
  runTag: string,
): Promise<Cosigner[]> {
  const cosigners: Cosigner[] = [];
  for (let index = 0; index < count; index += 1) {
    const midenClient = await MidenClient.create({
      rpcUrl: config.midenRpcEndpoint,
      // The network's prover, matching what the browser clients use. Local
      // in-WASM proving costs roughly seventy times the CPU and is available
      // as QUAL_TS_PROVER=local when the remote one is down.
      proverUrl: process.env.QUAL_TS_PROVER ?? config.network,
      storeName: `qual-${runTag}-${index}`,
      autoSync: false,
    });
    const multisigClient = new MultisigClient(midenClient, {
      guardianEndpoint: config.guardianEndpoint,
      midenRpcEndpoint: config.midenRpcEndpoint,
    });
    const { signer, secretKey } = makeSigner(scheme);
    cosigners.push({ signer, secretKey, midenClient, multisigClient });
  }
  return cosigners;
}

/**
 * GUARDIAN holds one acknowledgement identity per signature scheme, and the
 * account binds the one matching its own scheme. Asking without a scheme
 * returns the default, which an ECDSA account then rejects at registration.
 */
export async function guardianCommitment(cosigner: Cosigner, scheme: Scheme): Promise<string> {
  const response = await cosigner.multisigClient.guardianClient.getPubkey(scheme);
  return typeof response === 'string' ? response : response.commitment;
}

/** The account a live scenario is working on, shared by its actions. */
export interface LiveSession {
  readonly cosigners: Cosigner[];
  readonly threshold: number;
  multisig?: Multisig;
  accountId?: string;
  proposalId?: string;
  readonly scheme: Scheme;
  exportedProposal?: string;
  /** The signer being added, held aside so it cannot sign its own admission. */
  incoming?: Cosigner;
  /** The signer a removal took away, kept so its eviction can be tested. */
  departed?: Cosigner;
  expectedSigners?: string[];
  expectedThreshold?: number;
  faucetId?: string;
  treasuryId?: string;
  sentAmount?: bigint;
  balanceBeforeSend?: bigint;
  /** Set once a GUARDIAN migration is proposed: completion is judged differently. */
  migrating?: boolean;
  expectedProcedure?: string;
  expectedProcedureThreshold?: number;
  transferred?: bigint;
  /**
   * The nonce the producer's candidate occupies. Held from creation because
   * abandoning names the nonce, not the proposal, and the proposal is gone by
   * the time the abandon is asserted.
   */
  customNonce?: number;
  /**
   * The producer's own serialized request. Preparing execution re-executes it
   * against the signed commitment, so the same bytes have to survive the
   * scenario rather than being rebuilt.
   */
  customRequest?: Uint8Array;
  balanceSeen: boolean;
}

export function shapeOf(shape: string): { threshold: number; total: number } | null {
  const match = /^(\d+)-of-(\d+)$/.exec(shape);
  if (!match) return null;
  const threshold = Number(match[1]);
  const total = Number(match[2]);
  if (threshold === 0 || threshold > total) return null;
  return { threshold, total };
}
