import { beforeEach, describe, expect, it, vi } from 'vitest';

const { mockCreateClient } = vi.hoisted(() => ({
  mockCreateClient: vi.fn(),
}));

vi.mock('@miden-sdk/miden-sdk', () => ({
  WasmWebClient: {
    createClient: mockCreateClient,
  },
}));

import {
  compileTxScript,
  getRawMidenClient,
  getTransactionProver,
  requireConfigValue,
  setRawClientAdapter,
} from './raw-client.js';

describe('raw-client', () => {
  beforeEach(() => {
    mockCreateClient.mockReset();
  });

  it.each([undefined, null, 42, {}, []])(
    'rejects non-string configuration value %j consistently',
    (value) => {
      expect(() => requireConfigValue('guardianEndpoint', value)).toThrow(
        'missing required configuration: guardianEndpoint',
      );
    },
  );

  it('trims surrounding whitespace from configuration values', () => {
    expect(requireConfigValue('guardianEndpoint', '  http://localhost:3000\n')).toBe(
      'http://localhost:3000',
    );
  });

  // A `MidenClient` whose `_withInnerWebClient` behaves like the SDK's: it runs
  // `fn` with the wrapped WASM client and records whether a call is inside it.
  const publicClientWrapping = (inner: Record<string, unknown>) => {
    const slot = { depth: 0 };
    const withInnerWebClient = vi.fn(async (fn: (inner: unknown) => Promise<unknown>) => {
      slot.depth += 1;
      try {
        return await fn(inner);
      } finally {
        slot.depth -= 1;
      }
    });
    const client = {
      accounts: {},
      sync: vi.fn(),
      defaultProver: null,
      storeIdentifier: vi.fn(async () => 'browser-db'),
      _withInnerWebClient: withInnerWebClient,
    };
    return { client, slot, withInnerWebClient };
  };

  it('uses the WASM client the MidenClient wraps and opens no second client', async () => {
    const inner = {
      getAccount: vi.fn(async function (this: unknown, id: string) {
        return { id, self: this };
      }),
    };
    const { client } = publicClientWrapping(inner);

    const rawClient = await getRawMidenClient(client as any);
    const account = await rawClient.getAccount('0xabc' as any);

    expect(inner.getAccount).toHaveBeenCalledWith('0xabc');
    expect(account).toEqual({ id: '0xabc', self: inner });
    // A second client on the same store keeps its own storage-map trees and
    // can persist a stale root (issue #481).
    expect(mockCreateClient).not.toHaveBeenCalled();
  });

  it("runs every call inside the MidenClient's own queue", async () => {
    const depthDuringCall: number[] = [];
    const { client, slot, withInnerWebClient } = publicClientWrapping({
      syncState: vi.fn(async () => {
        depthDuringCall.push(slot.depth);
        return { blockNum: 7 };
      }),
    });

    const rawClient = await getRawMidenClient(client as any);
    const callsBefore = withInnerWebClient.mock.calls.length;
    await expect(rawClient.syncState()).resolves.toEqual({ blockNum: 7 });

    expect(depthDuringCall).toEqual([1]);
    expect(withInnerWebClient.mock.calls.length).toBe(callsBefore + 1);
    expect(slot.depth).toBe(0);
  });

  it('refuses a MidenClient that does not expose its WASM client', async () => {
    const client = {
      accounts: {},
      sync: vi.fn(),
      defaultProver: null,
      storeIdentifier: vi.fn(async () => 'browser-db'),
    };

    await expect(getRawMidenClient(client as any)).rejects.toThrow(
      'MidenClient does not expose _withInnerWebClient',
    );
    expect(mockCreateClient).not.toHaveBeenCalled();
  });

  it('returns an injected raw web client without needing an endpoint', async () => {
    const rawClient = {
      executeTransaction: vi.fn(),
      proveTransaction: vi.fn(),
    };

    await expect(getRawMidenClient(rawClient as any)).resolves.toBe(rawClient);
    expect(mockCreateClient).not.toHaveBeenCalled();
  });

  it('returns the default prover from a public MidenClient', () => {
    const prover = { kind: 'devnet-prover' };
    const client = {
      accounts: {},
      sync: vi.fn(),
      defaultProver: prover,
      storeIdentifier: vi.fn(() => 'browser-db'),
    };

    expect(getTransactionProver(client as any)).toBe(prover);
  });

  it('returns null for raw web clients', () => {
    const rawClient = {
      executeTransaction: vi.fn(),
      proveTransaction: vi.fn(),
    };

    expect(getTransactionProver(rawClient as any)).toBeNull();
  });

  it('caches the shared raw client for a public MidenClient', async () => {
    const { client } = publicClientWrapping({ getAccount: vi.fn() });

    const first = await getRawMidenClient(client as any);
    const second = await getRawMidenClient(client as any);

    expect(second).toBe(first);
    expect(mockCreateClient).not.toHaveBeenCalled();
  });

  it("sends the adapter's operations to the adapter and the rest to the wrapped client", async () => {
    const inner = { getAccount: vi.fn(async () => 'inner-account'), getSyncHeight: vi.fn(async () => 9) };
    const { client, withInnerWebClient } = publicClientWrapping(inner);
    const rawClient = await getRawMidenClient(client as any);
    // Set after the raw client exists: the adapter is read on each call.
    const adapter = { getAccount: vi.fn(async () => 'writer-account') };
    setRawClientAdapter(client as any, adapter as any);
    const callsBefore = withInnerWebClient.mock.calls.length;

    await expect(rawClient.getAccount('0xabc' as any)).resolves.toBe('writer-account');
    expect(adapter.getAccount).toHaveBeenCalledWith('0xabc');
    expect(inner.getAccount).not.toHaveBeenCalled();
    expect(withInnerWebClient.mock.calls.length).toBe(callsBefore);

    await expect(rawClient.getSyncHeight()).resolves.toBe(9);
    expect(withInnerWebClient.mock.calls.length).toBe(callsBefore + 1);
  });

  it('uses the replacement when the adapter is set again', async () => {
    const { client } = publicClientWrapping({});
    const rawClient = await getRawMidenClient(client as any);
    setRawClientAdapter(client as any, { syncState: vi.fn(async () => 'first') } as any);
    setRawClientAdapter(client as any, { syncState: vi.fn(async () => 'second') } as any);

    await expect(rawClient.syncState()).resolves.toBe('second');
  });

  it('uses the public compile resource when available', async () => {
    const script = { kind: 'compiled-script' };
    const client = {
      accounts: {},
      sync: vi.fn(),
      compile: {
        txScript: vi.fn().mockResolvedValue(script),
      },
      storeIdentifier: vi.fn(() => 'browser-db'),
    };

    await expect(
      compileTxScript(
        client as any,
        'begin end',
        [{ namespace: 'auth::multisig', code: 'export.foo' }],
      ),
    ).resolves.toBe(script);

    expect(client.compile.txScript).toHaveBeenCalledWith({
      code: 'begin end',
      libraries: [{ namespace: 'auth::multisig', code: 'export.foo' }],
    });
    expect(mockCreateClient).not.toHaveBeenCalled();
  });

  it('falls back to raw client compilation for low-level callers', async () => {
    const compiledScript = { kind: 'compiled-script' };
    const builtLibrary = { kind: 'built-library' };
    const builder = {
      buildLibrary: vi.fn().mockReturnValue(builtLibrary),
      linkDynamicLibrary: vi.fn(),
      linkStaticLibrary: vi.fn(),
      compileTxScript: vi.fn().mockReturnValue(compiledScript),
    };
    const rawClient = {
      createCodeBuilder: vi.fn().mockReturnValue(builder),
    };

    await expect(
      compileTxScript(
        rawClient as any,
        'begin end',
        [
          { namespace: 'auth::multisig', code: 'export.foo' },
          { namespace: 'auth::guardian', code: 'export.bar', linking: 'static' },
        ],
      ),
    ).resolves.toBe(compiledScript);

    expect(builder.buildLibrary).toHaveBeenNthCalledWith(1, 'auth::multisig', 'export.foo');
    expect(builder.buildLibrary).toHaveBeenNthCalledWith(2, 'auth::guardian', 'export.bar');
    expect(builder.linkDynamicLibrary).toHaveBeenCalledWith(builtLibrary);
    expect(builder.linkStaticLibrary).toHaveBeenCalledWith(builtLibrary);
    expect(builder.compileTxScript).toHaveBeenCalledWith('begin end');
  });
});
