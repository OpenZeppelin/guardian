import type * as MidenSdk from "@demox-labs/miden-sdk";
import { PsmHttpClient } from "@openzeppelin/psm-client";
import { createMultisigAccount } from "../multisig/account-builder";
import type { MultisigConfig } from "../multisig/types";

const RPC_ENDPOINT = "https://rpc.testnet.miden.io:443";

export async function clearIndexedDB() {
  const databases = await indexedDB.databases();
  const deletePromises = databases
    .filter((db) => db.name)
    .map(
      (db) =>
        new Promise<void>((resolve, reject) => {
          const request = indexedDB.deleteDatabase(db.name!);
          request.onsuccess = () => resolve();
          request.onerror = () => reject(request.error);
          request.onblocked = () => resolve();
        })
    );
  await Promise.all(deletePromises);
}

export interface MidenClientResult {
  client: MidenSdk.WebClient;
  sdk: typeof MidenSdk;
}

export async function createMidenClient(): Promise<MidenClientResult> {
  const sdk = await import("@demox-labs/miden-sdk");

  const client = await sdk.WebClient.createClient(RPC_ENDPOINT);

  await client.syncState();
  return { client, sdk };
}

/**
 * Fetches the PSM server pubkey commitment, builds a multisig+PSM account
 * using the same MASM templates as the Rust contracts, and returns the
 * account plus seed. Caller can add it to the WebClient or register on PSM.
 */
export async function createMultisigWithPsm({
  client,
  sdk,
  psmEndpoint,
  threshold,
  signerCommitments,
  psmEnabled = true,
}: {
  client: MidenSdk.WebClient;
  sdk: typeof MidenSdk;
  psmEndpoint: string;
  threshold: number;
  signerCommitments: string[]; // hex words
  psmEnabled?: boolean;
}) {
  const psmHttp = new PsmHttpClient(psmEndpoint);
  const psmPubkeyHex = await psmHttp.getPubkey();

  const config: MultisigConfig = {
    threshold,
    signerCommitments,
    psmCommitment: psmPubkeyHex,
    psmEnabled,
  };

  const { account, seed } = await createMultisigAccount(client, sdk, config);
  return { account, seed };
}
