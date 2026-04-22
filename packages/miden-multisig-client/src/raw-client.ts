import {
  MidenClient,
  type TransactionProver,
  type TransactionScript,
  WasmWebClient,
} from '@miden-sdk/miden-sdk';

export const DEFAULT_MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';

export type RawClientSource = MidenClient | WasmWebClient;
export interface ScriptLibrarySource {
  namespace: string;
  code: string;
  linking?: 'dynamic' | 'static';
}

const rawClientCache = new WeakMap<MidenClient, Promise<WasmWebClient>>();

export function resolveMidenRpcEndpoint(endpoint?: string): string {
  return endpoint ?? DEFAULT_MIDEN_RPC_URL;
}

function isPublicMidenClient(client: RawClientSource): client is MidenClient {
  return 'accounts' in client && 'sync' in client;
}

export function getRawMidenClient(
  client: RawClientSource,
  rpcUrl?: string,
): Promise<WasmWebClient> {
  if (!isPublicMidenClient(client)) {
    return Promise.resolve(client);
  }

  const cached = rawClientCache.get(client);
  if (cached) {
    return cached;
  }

  const rawClient = WasmWebClient.createClient(
    resolveMidenRpcEndpoint(rpcUrl),
    undefined,
    undefined,
    client.storeIdentifier(),
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
  const builder = rawClient.createCodeBuilder();
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
