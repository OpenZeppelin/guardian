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

const MIDEN_RPC_SHORTHANDS: Record<string, string> = {
  local: 'http://localhost:57291',
  localhost: 'http://localhost:57291',
  devnet: 'https://rpc.devnet.miden.io',
  testnet: 'https://rpc.testnet.miden.io',
  cedar: 'https://rpc.cedar.miden.io',
  mainnet: 'https://rpc.mainnet.miden.io',
};

/**
 * Resolves a network shorthand (`local`, `devnet`, `testnet`, `cedar`,
 * `mainnet`) to the node's full RPC URL; any other value is returned trimmed.
 * Resolve once and pass the result to both `createMidenClient` and the
 * multisig SDK, whose direct chain reads need a full URL.
 */
export function resolveMidenRpcUrl(rpcUrl: string): string {
  const trimmed = rpcUrl.trim();
  return MIDEN_RPC_SHORTHANDS[trimmed.toLowerCase()] ?? trimmed;
}

/**
 * Creates the Miden client with `useWorker: false`: in worker mode, transactions
 * execute and apply in a Web Worker that never sees the account state the
 * multisig SDK loads or syncs from GUARDIAN
 * (https://github.com/0xMiden/web-sdk/issues/441).
 */
export async function createMidenClient(
  rpcUrl: string,
  storeName = 'MidenClientDB',
): Promise<MidenClient> {
  const resolvedRpcUrl = resolveMidenRpcUrl(rpcUrl);
  const normalizedRpcUrl = resolvedRpcUrl.toLowerCase();
  const useLocalProver = prefersLocalProver();
  if (normalizedRpcUrl === MIDEN_RPC_SHORTHANDS.devnet) {
    if (useLocalProver) {
      return MidenClient.create({
        rpcUrl: resolvedRpcUrl,
        noteTransportUrl: 'devnet',
        proverUrl: 'local',
        storeName,
        autoSync: true,
        useWorker: false,
      });
    }
    return MidenClient.createDevnet({ rpcUrl: resolvedRpcUrl, storeName, useWorker: false });
  }

  if (normalizedRpcUrl === MIDEN_RPC_SHORTHANDS.testnet) {
    if (useLocalProver) {
      return MidenClient.create({
        rpcUrl: resolvedRpcUrl,
        noteTransportUrl: 'testnet',
        proverUrl: 'local',
        storeName,
        autoSync: true,
        useWorker: false,
      });
    }
    return MidenClient.createTestnet({ rpcUrl: resolvedRpcUrl, storeName, useWorker: false });
  }

  if (normalizedRpcUrl === MIDEN_RPC_SHORTHANDS.cedar) {
    return MidenClient.create({
      rpcUrl: resolvedRpcUrl,
      noteTransportUrl: 'https://transport.cedar.miden.io',
      proverUrl: useLocalProver ? 'local' : 'https://tx-prover.cedar.miden.io',
      storeName,
      autoSync: true,
      useWorker: false,
    });
  }

  if (normalizedRpcUrl === MIDEN_RPC_SHORTHANDS.mainnet) {
    return MidenClient.create({
      rpcUrl: resolvedRpcUrl,
      noteTransportUrl: 'https://transport.mainnet.miden.io',
      proverUrl: useLocalProver ? 'local' : 'https://tx-prover.mainnet.miden.io',
      storeName,
      autoSync: true,
      useWorker: false,
    });
  }

  return MidenClient.create({
    rpcUrl: resolvedRpcUrl,
    proverUrl: normalizedRpcUrl === MIDEN_RPC_SHORTHANDS.local ? 'local' : undefined,
    storeName,
    autoSync: true,
    useWorker: false,
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
