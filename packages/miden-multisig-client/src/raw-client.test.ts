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

  it('rejects shadow client creation without an RPC endpoint', async () => {
    const client = {
      accounts: {},
      sync: vi.fn(),
      defaultProver: null,
      storeIdentifier: vi.fn(() => 'browser-db'),
    };

    await expect(getRawMidenClient(client as any)).rejects.toThrow(
      'missing required configuration: midenRpcEndpoint',
    );
    await expect(getRawMidenClient(client as any, '   ')).rejects.toThrow(
      'missing required configuration: midenRpcEndpoint',
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

  it('creates and caches a raw client for a public MidenClient', async () => {
    const rawClient = { kind: 'raw-client' };
    const client = {
      accounts: {},
      sync: vi.fn(),
      defaultProver: null,
      storeIdentifier: vi.fn(() => 'browser-db'),
    };

    mockCreateClient.mockResolvedValue(rawClient);

    await expect(
      getRawMidenClient(client as any, 'https://rpc.devnet.miden.io'),
    ).resolves.toBe(rawClient);
    await expect(
      getRawMidenClient(client as any, 'https://rpc.devnet.miden.io'),
    ).resolves.toBe(rawClient);

    expect(mockCreateClient).toHaveBeenCalledTimes(1);
    expect(mockCreateClient).toHaveBeenCalledWith(
      'https://rpc.devnet.miden.io',
      undefined,
      undefined,
      'browser-db',
    );
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
