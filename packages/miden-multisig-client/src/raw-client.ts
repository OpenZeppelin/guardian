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

export function requireConfigValue(field: string, value?: string): string {
  if (value === undefined || value.trim() === '') {
    throw new Error(`missing required configuration: ${field}`);
  }
  return value;
}

export function requireMidenRpcEndpoint(endpoint?: string): string {
  return requireConfigValue('midenRpcEndpoint', endpoint);
}

function isPublicMidenClient(client: RawClientSource): client is MidenClient {
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
  const rawClient = WasmWebClient.createClient(
    endpoint,
    undefined,
    undefined,
    await client.storeIdentifier(),
  );
  rawClientCache.set(client, rawClient);
  return rawClient;
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
