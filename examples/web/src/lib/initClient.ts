import {
  MultisigClient,
  FalconSigner,
  setMasmBaseUrl,
} from '@openzeppelin/miden-multisig-client';
import { WebClient, SecretKey } from '@demox-labs/miden-sdk';
import { MASM_BASE_URL, MIDEN_DB_NAME, MIDEN_RPC_URL, PSM_ENDPOINT } from '@/config';
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

export async function initializeSigner(webClient: WebClient): Promise<SignerInfo> {
  const secretKey = SecretKey.rpoFalconWithRNG(undefined);
  try {
    await webClient.addAccountSecretKeyToWebStore(secretKey);
  } catch {
    // Key may already exist on reload; ignore
  }
  const publicKey = secretKey.publicKey();
  const commitment = publicKey.toCommitment().toHex();
  return { commitment, secretKey };
}

export async function initClients(psmEndpoint = PSM_ENDPOINT) {
  setMasmBaseUrl(MASM_BASE_URL);
  const webClient = await createWebClient();
  const signerInfo = await initializeSigner(webClient);

  const multisigClient = new MultisigClient(webClient, { psmEndpoint });
  const psmPubkey = await multisigClient.psmClient.getPubkey();

  const falconSigner = new FalconSigner(signerInfo.secretKey);

  return { webClient, multisigClient, signerInfo, falconSigner, psmPubkey };
}

