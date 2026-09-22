import {
  MidenClient,
  type TransactionProver,
  type TransactionScript,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';

export type RawClientSource = MidenClient | WasmWebClient;
export interface ScriptLibrarySource {
  namespace: string;
  code: string;
  linking?: 'dynamic' | 'static';
}

const rawClientCache = new WeakMap<MidenClient, Promise<WasmWebClient>>();

export function requireConfigValue(field: string, value?: unknown): string {
  if (typeof value !== 'string') {
    throw new Error(`missing required configuration: ${field}`);
  }
  const normalizedValue = value.trim();
  if (normalizedValue === '') {
    throw new Error(`missing required configuration: ${field}`);
  }
  return normalizedValue;
}

export function requireMidenRpcEndpoint(endpoint?: string): string {
  return requireConfigValue('midenRpcEndpoint', endpoint);
}

export function isPublicMidenClient(client: RawClientSource): client is MidenClient {
  return 'accounts' in client && 'sync' in client;
}

export async function getRawMidenClient(
  client: RawClientSource,
  rpcUrl?: string,
): Promise<WasmWebClient> {
  if (!isPublicMidenClient(client)) {
    return client;
  }

  const cached = rawClientCache.get(client);
  if (cached) {
    return cached;
  }

  const endpoint = requireMidenRpcEndpoint(rpcUrl);
  const rawClient = createRawClient(client, endpoint);
  rawClientCache.set(client, rawClient);
  return rawClient;
}

/**
 * Opens the WASM client behind `client` on the same store.
 *
 * Since 0.17 a client needs the chain's fee faucet to build its protocol
 * configuration, and creation fails without one for any network the SDK has no
 * preset for. The parent client already resolved it, so it is read back from
 * there rather than asked of the caller a second time. It is the eighth
 * argument of `createClient`; the ones between are left at their defaults.
 */
async function createRawClient(client: MidenClient, endpoint: string): Promise<WasmWebClient> {
  const [storeName, feeFaucetId] = await Promise.all([
    client.storeIdentifier(),
    client.feeFaucetId(),
  ]);
  return WasmWebClient.createClient(
    endpoint,
    undefined,
    undefined,
    storeName,
    undefined,
    undefined,
    undefined,
    feeFaucetId?.toString(),
  );
}

export function getTransactionProver(client: RawClientSource): TransactionProver | null {
  return isPublicMidenClient(client) ? client.defaultProver : null;
}

export async function compileTxScript(
  client: RawClientSource,
  code: string,
  libraries: ScriptLibrarySource[] = [],
  rpcUrl?: string,
): Promise<TransactionScript> {
  if (isPublicMidenClient(client)) {
    return client.compile.txScript({ code, libraries });
  }

  const rawClient = await getRawMidenClient(client, rpcUrl);
  const builder = await rawClient.createCodeBuilder();
  for (const library of libraries) {
    const builtLibrary = builder.buildLibrary(library.namespace, library.code);
    if (library.linking === 'static') {
      builder.linkStaticLibrary(builtLibrary);
    } else {
      builder.linkDynamicLibrary(builtLibrary);
    }
  }
  return builder.compileTxScript(code);
}
