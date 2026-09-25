import {
  MidenClient,
  type SyncSummary,
  type TransactionProver,
  type TransactionScript,
  type WasmWebClient,
} from '@miden-sdk/miden-sdk';

export type RawClientSource = MidenClient | WasmWebClient;
export interface ScriptLibrarySource {
  namespace: string;
  code: string;
  linking?: 'dynamic' | 'static';
}

/**
 * The account state operations an application can send to its own writer.
 * Use it when the client that writes the account is not the `MidenClient`
 * given to this SDK, for example a client in another JS realm. Then the SDK
 * does not write the account through a second client.
 *
 * It does not cover every call. Note reads (`getConsumableNotes`,
 * `getInputNote`, `getInputNotes`, `getOutputNote`) still go to the wrapped
 * client, and `executeProposal` runs through the public
 * `MidenClient.transactions.executeRequest`, which this hook cannot reach.
 */
export interface RawClientAdapter {
  getAccount: WasmWebClient['getAccount'];
  newAccount: WasmWebClient['newAccount'];
  syncState(): Promise<SyncSummary>;
  syncChain(): Promise<SyncSummary>;
  chainAnchorForRequest: WasmWebClient['chainAnchorForRequest'];
  executeForSummary: WasmWebClient['executeForSummary'];
  executeForSummaryAt: WasmWebClient['executeForSummaryAt'];
  importNoteFile: WasmWebClient['importNoteFile'];
}

const rawClientCache = new WeakMap<MidenClient, Promise<WasmWebClient>>();
const rawClientAdapters = new WeakMap<MidenClient, RawClientAdapter>();

/** Sends the adapter's operations for `client` to the adapter. A later call replaces it. */
export function setRawClientAdapter(client: MidenClient, adapter: RawClientAdapter): void {
  rawClientAdapters.set(client, adapter);
}

export function getRawClientAdapter(client: MidenClient): RawClientAdapter | undefined {
  return rawClientAdapters.get(client);
}

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

export async function getRawMidenClient(client: RawClientSource): Promise<WasmWebClient> {
  if (!isPublicMidenClient(client)) {
    return client;
  }

  const cached = rawClientCache.get(client);
  if (cached) {
    return cached;
  }

  const rawClient = shareInnerWebClient(client);
  rawClientCache.set(client, rawClient);
  return rawClient;
}

/**
 * The SDK's access to the WASM client that a `MidenClient` wraps. It is
 * `@internal` in the SDK and not in its typings, so it is declared here.
 */
interface InnerWebClientAccess {
  _withInnerWebClient<T>(fn: (inner: WasmWebClient) => Promise<T>): Promise<T>;
}

function hasInnerWebClientAccess(client: MidenClient): client is MidenClient & InnerWebClientAccess {
  return typeof Reflect.get(client, '_withInnerWebClient') === 'function';
}

/**
 * Returns the WASM client that `client` already wraps. It does not open a
 * second client on the same store.
 *
 * A second client keeps its own in-memory account and storage-map trees. When
 * the two clients both write one account, each one computes the next storage
 * root from its own trees, and a stale tree persists a root that omits an
 * entry the other client wrote (issue #481). Sharing one store or holding an
 * outer mutex does not prevent this. Only one client may write the account.
 *
 * Each method call runs inside `_withInnerWebClient`, so it joins the queue
 * that every other call on `client` uses. A method that the client's adapter
 * supplies (`setRawClientAdapter`) goes to the adapter instead. The adapter is
 * read on each call, so an adapter set after this returns still applies.
 */
async function shareInnerWebClient(client: MidenClient): Promise<WasmWebClient> {
  if (!hasInnerWebClientAccess(client)) {
    throw new Error('MidenClient does not expose _withInnerWebClient; this SDK version is not supported');
  }
  const withInner = <T>(fn: (inner: WasmWebClient) => Promise<T>): Promise<T> =>
    client._withInnerWebClient(fn);
  // Used only to read properties. Every call runs on the client the SDK
  // passes to `_withInnerWebClient`.
  const inner = await withInner(async current => current);
  return new Proxy(inner, {
    get(target, property) {
      const adapter = rawClientAdapters.get(client);
      const override: unknown = adapter ? Reflect.get(adapter, property) : undefined;
      if (typeof override === 'function') {
        return (...args: unknown[]) => Reflect.apply(override, adapter, args);
      }
      const value: unknown = Reflect.get(target, property);
      if (typeof value !== 'function') {
        return value;
      }
      return (...args: unknown[]) => withInner(async current => Reflect.apply(value, current, args));
    },
  });
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

  const rawClient = await getRawMidenClient(client);
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
