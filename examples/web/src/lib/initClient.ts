import { WebClient, AuthSecretKey } from '@miden-sdk/miden-sdk';
import { MIDEN_DB_NAME, MIDEN_RPC_URL } from '@/config';
import type { SignerInfo } from '@/types';

export async function clearMidenDatabase(dbName = MIDEN_DB_NAME): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.deleteDatabase(dbName);
    request.onsuccess = () => resolve();
    request.onerror = () => reject(request.error);
    request.onblocked = () => resolve();
  });
}

export async function createWebClient(rpcUrl = MIDEN_RPC_URL): Promise<WebClient> {
  const client = await WebClient.createClient(rpcUrl);
  await client.syncState();
  return client;
}

export async function initializeSigner(_webClient: WebClient): Promise<SignerInfo> {
  const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
  // Note: In v0.13, addAccountSecretKeyToWebStore requires an account_id.
  // The key is stored in memory and will be associated with accounts when they're created.
  const publicKey = secretKey.publicKey();
  const commitment = publicKey.toCommitment().toHex();
  return { commitment, secretKey };
}
