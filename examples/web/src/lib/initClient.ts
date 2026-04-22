import { MidenClient, AuthSecretKey } from '@miden-sdk/miden-sdk';
import { EcdsaSigner, FalconSigner } from '@openzeppelin/miden-multisig-client';
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

export async function createMidenClient(rpcUrl = MIDEN_RPC_URL): Promise<MidenClient> {
  const normalizedRpcUrl = rpcUrl.trim().toLowerCase();
  if (normalizedRpcUrl === 'devnet' || normalizedRpcUrl === 'https://rpc.devnet.miden.io') {
    return MidenClient.createDevnet({ rpcUrl, storeName: MIDEN_DB_NAME });
  }

  if (normalizedRpcUrl === 'testnet' || normalizedRpcUrl === 'https://rpc.testnet.miden.io') {
    return MidenClient.createTestnet({ rpcUrl, storeName: MIDEN_DB_NAME });
  }

  return MidenClient.create({
    rpcUrl,
    proverUrl:
      normalizedRpcUrl === 'local' ||
      normalizedRpcUrl === 'localhost' ||
      normalizedRpcUrl === 'http://localhost:57291'
        ? 'local'
        : undefined,
    storeName: MIDEN_DB_NAME,
    autoSync: true,
  });
}

export async function initializeSigner(_midenClient: MidenClient): Promise<SignerInfo> {
  const falconSecretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
  const ecdsaSecretKey = AuthSecretKey.ecdsaWithRNG(undefined);

  const falconSigner = new FalconSigner(falconSecretKey);
  const ecdsaSigner = new EcdsaSigner(ecdsaSecretKey);

  return {
    falcon: {
      commitment: falconSigner.commitment,
      secretKey: falconSecretKey,
    },
    ecdsa: {
      commitment: ecdsaSigner.commitment,
      secretKey: ecdsaSecretKey,
    },
    activeScheme: 'falcon',
  };
}
