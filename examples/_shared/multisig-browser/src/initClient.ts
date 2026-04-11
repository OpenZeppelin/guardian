import { AuthSecretKey, MidenClient } from '@miden-sdk/miden-sdk';
import { EcdsaSigner, FalconSigner } from '@openzeppelin/miden-multisig-client';
import type { SignerInfo } from './types';

function deleteDatabase(name: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.deleteDatabase(name);
    request.onsuccess = () => resolve();
    request.onerror = () => reject(request.error);
    request.onblocked = () => resolve();
  });
}

export async function clearIndexedDbDatabase(name: string): Promise<void> {
  await deleteDatabase(name);
}

export async function clearIndexedDbDatabasesByPrefix(prefixes: string[]): Promise<void> {
  if (prefixes.length === 0) {
    return;
  }

  const databases = await indexedDB.databases();
  const matchingNames = databases
    .map((database) => database.name)
    .filter(
      (name): name is string =>
        typeof name === 'string' &&
        prefixes.some((prefix) => name === prefix || name.startsWith(`${prefix}_`)),
    );

  const uniqueNames = [...new Set(matchingNames)];
  await Promise.all(uniqueNames.map((name) => deleteDatabase(name)));
}

export async function clearIndexedDbDatabases(names?: string[]): Promise<void> {
  if (names && names.length > 0) {
    await Promise.all(names.map((name) => deleteDatabase(name)));
    return;
  }

  const databases = await indexedDB.databases();
  await Promise.all(
    databases
      .filter((database) => database.name)
      .map((database) => deleteDatabase(database.name!)),
  );
}

function prefersLocalProver(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  const prover = new URLSearchParams(window.location.search).get('prover');
  return prover?.trim().toLowerCase() === 'local';
}

export async function createMidenClient(
  rpcUrl: string,
  storeName = 'MidenClientDB',
): Promise<MidenClient> {
  const normalizedRpcUrl = rpcUrl.trim().toLowerCase();
  const useLocalProver = prefersLocalProver();
  if (normalizedRpcUrl === 'devnet' || normalizedRpcUrl === 'https://rpc.devnet.miden.io') {
    if (useLocalProver) {
      return MidenClient.create({
        rpcUrl,
        noteTransportUrl: 'devnet',
        proverUrl: 'local',
        storeName,
        autoSync: true,
      });
    }
    return MidenClient.createDevnet({ rpcUrl, storeName });
  }

  if (normalizedRpcUrl === 'testnet' || normalizedRpcUrl === 'https://rpc.testnet.miden.io') {
    if (useLocalProver) {
      return MidenClient.create({
        rpcUrl,
        noteTransportUrl: 'testnet',
        proverUrl: 'local',
        storeName,
        autoSync: true,
      });
    }
    return MidenClient.createTestnet({ rpcUrl, storeName });
  }

  return MidenClient.create({
    rpcUrl,
    proverUrl:
      normalizedRpcUrl === 'local' ||
      normalizedRpcUrl === 'localhost' ||
      normalizedRpcUrl === 'http://localhost:57291'
        ? 'local'
        : undefined,
    storeName,
    autoSync: true,
  });
}

export async function initializeLocalSigners(): Promise<SignerInfo> {
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
