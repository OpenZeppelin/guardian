import { describe, it, expect, vi, beforeEach } from 'vitest';
import { MultisigClient } from './client.js';
import type { Signer } from './types.js';

// Mock the Miden SDK
vi.mock('@miden-sdk/miden-sdk', () => ({
  AccountId: {
    fromHex: vi.fn((hex: string) => ({ toString: () => hex })),
  },
  Account: {
    deserialize: vi.fn(() => ({
      id: () => ({
        toString: () => '0x' + 'd'.repeat(30),
        prefix: () => ({ asInt: () => BigInt(1) }),
        suffix: () => ({ asInt: () => BigInt(2) }),
      }),
      serialize: () => new Uint8Array([1, 2, 3]),
      storage: vi.fn(),
      vault: vi.fn(),
    })),
  },
}));

// Mock the AccountInspector
vi.mock('./inspector.js', () => ({
  AccountInspector: {
    fromAccount: vi.fn(() => ({
      threshold: 2,
      numSigners: 2,
      signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
      guardianEnabled: true,
      guardianCommitment: '0x' + 'c'.repeat(64),
      vaultBalances: [],
      procedureThresholds: new Map(),
    })),
  },
}));

// Mock the account creation module
vi.mock('./account/index.js', () => ({
  createMultisigAccount: vi.fn().mockResolvedValue({
    account: {
      id: () => ({
        toString: () => '0x' + 'a'.repeat(30),
        prefix: () => ({ asInt: () => BigInt(1) }),
        suffix: () => ({ asInt: () => BigInt(2) }),
      }),
      serialize: () => new Uint8Array([1, 2, 3]),
    },
    seed: new Uint8Array([4, 5, 6]),
  }),
}));

// Mock fetch for GUARDIAN client
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

const GUARDIAN_URL = 'http://localhost:3000';
const MIDEN_RPC = 'http://localhost:57291';
const CLIENT_CONFIG = { guardianEndpoint: GUARDIAN_URL, midenRpcEndpoint: MIDEN_RPC };

describe('MultisigClient', () => {
  let webClient: any;
  let mockSigner: Signer;

  beforeEach(() => {
    mockFetch.mockReset();

    webClient = {
      accounts: {
        get: vi.fn().mockResolvedValue(null),
        insert: vi.fn().mockResolvedValue(undefined),
      },
      keystore: {
        insert: vi.fn().mockResolvedValue(undefined),
      },
    };

    mockSigner = {
      commitment: '0x' + '1'.repeat(64),
      publicKey: '0x' + '2'.repeat(64),
      scheme: 'falcon',
      signAccountIdWithTimestamp: vi.fn().mockResolvedValue('0x' + 'a'.repeat(128)),
      signRequest: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
      signCommitment: vi.fn().mockReturnValue('0x' + 'b'.repeat(128)),
    };
  });

  describe('constructor', () => {
    it('should create client when both endpoints are supplied', () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      expect(client).toBeInstanceOf(MultisigClient);
    });

    it('should create client with custom GUARDIAN endpoint', () => {
      const client = new MultisigClient(webClient, {
        guardianEndpoint: 'http://custom:8080',
        midenRpcEndpoint: MIDEN_RPC,
      });
      expect(client).toBeInstanceOf(MultisigClient);
    });

    it('throws when the config object is omitted', () => {
      expect(() => new (MultisigClient as any)(webClient)).toThrow(
        'missing required configuration: midenRpcEndpoint',
      );
    });

    it.each([undefined, null, 42, '', '   '])(
      'throws before any network or store access when midenRpcEndpoint is %j',
      (endpoint) => {
        expect(
          () =>
            new MultisigClient(webClient, {
              guardianEndpoint: GUARDIAN_URL,
              midenRpcEndpoint: endpoint as any,
            }),
        ).toThrow('missing required configuration: midenRpcEndpoint');
        expect(mockFetch).not.toHaveBeenCalled();
        expect(webClient.accounts.get).not.toHaveBeenCalled();
        expect(webClient.accounts.insert).not.toHaveBeenCalled();
      },
    );

    it.each([undefined, null, 42, '', '   '])(
      'throws before any network or store access when guardianEndpoint is %j',
      (endpoint) => {
        expect(
          () =>
            new MultisigClient(webClient, {
              guardianEndpoint: endpoint as any,
              midenRpcEndpoint: MIDEN_RPC,
            }),
        ).toThrow('missing required configuration: guardianEndpoint');
        expect(mockFetch).not.toHaveBeenCalled();
        expect(webClient.accounts.get).not.toHaveBeenCalled();
        expect(webClient.accounts.insert).not.toHaveBeenCalled();
      },
    );

    it('rejects a blank endpoint passed to setGuardianEndpoint', () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      expect(() => client.setGuardianEndpoint('   ')).toThrow(
        'missing required configuration: guardianEndpoint',
      );
    });
  });

  describe('guardianClient getter', () => {
    it('should expose GUARDIAN client for getting pubkey', () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      expect(client.guardianClient).toBeDefined();
    });
  });

  describe('create', () => {
    it('should create multisig and return Multisig instance', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = await client.create(config, mockSigner);

      expect(multisig).toBeDefined();
      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual(config.signerCommitments);
      expect(multisig.guardianCommitment).toBe(config.guardianCommitment);
    });

    it('should set signer on GUARDIAN client', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = await client.create(config, mockSigner);
      expect(multisig.signerCommitment).toBe(mockSigner.commitment);
    });

    it('binds the signer auth key to the created account when supported', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      const bindAccountKey = vi.fn().mockResolvedValue(undefined);
      const bindingSigner = {
        ...mockSigner,
        bindAccountKey,
      };

      await client.create({
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      }, bindingSigner);

      expect(bindAccountKey).toHaveBeenCalledWith(webClient, '0x' + 'a'.repeat(30));
    });
  });

  describe('load', () => {
    it('should load existing multisig account and detect config', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      // Mock getState response
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: '0x' + 'd'.repeat(30),
          commitment: '0x' + 'e'.repeat(64),
          state_json: { data: 'base64state' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      const accountId = '0x' + 'd'.repeat(30);
      const multisig = await client.load(accountId, mockSigner);

      expect(multisig).toBeDefined();
      expect(multisig.accountId).toBe(accountId);
      // Config is detected from account storage via AccountInspector
      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual(['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)]);
      expect(multisig.guardianCommitment).toBe('0x' + 'c'.repeat(64));
      expect(multisig.account).not.toBeNull();
      expect(webClient.accounts.get).toHaveBeenCalledTimes(1);
      expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
    });

    it('should throw if account not found on GUARDIAN', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 404,
        statusText: 'Not Found',
        text: async () => 'Account not found',
      });

      await expect(
        client.load('0xnonexistent', mockSigner)
      ).rejects.toThrow();
    });

    it('should allow registerOnGuardian after load without explicit initial state', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: '0x' + 'd'.repeat(30),
          commitment: '0x' + 'e'.repeat(64),
          state_json: { data: 'base64state' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: true,
          message: 'Account configured',
          ack_pubkey: '0x' + 'f'.repeat(64),
        }),
      });

      const accountId = '0x' + 'd'.repeat(30);
      const multisig = await client.load(accountId, mockSigner);

      await expect(multisig.registerOnGuardian()).resolves.toBeUndefined();
      expect(webClient.accounts.get).toHaveBeenCalledTimes(1);
      expect(webClient.accounts.insert).toHaveBeenCalledTimes(1);
    });

    it('binds the signer auth key after loading an account when supported', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      const bindAccountKey = vi.fn().mockResolvedValue(undefined);
      const bindingSigner = {
        ...mockSigner,
        bindAccountKey,
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: '0x' + 'd'.repeat(30),
          commitment: '0x' + 'e'.repeat(64),
          state_json: { data: 'base64state' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      await client.load('0x' + 'd'.repeat(30), bindingSigner);

      expect(bindAccountKey).toHaveBeenCalledWith(webClient, '0x' + 'd'.repeat(30));
    });
  });

  // --- recoverByKey -------------------

  describe('recoverByKey', () => {
    function makeLookupCapableSigner() {
      return {
        commitment: '0x' + 'a'.repeat(64),
        publicKey: '0x' + 'p'.repeat(897),
        scheme: 'falcon' as const,
        signAccountIdWithTimestamp: vi.fn().mockResolvedValue('0x' + 'a'.repeat(128)),
        signRequest: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
        signCommitment: vi.fn().mockReturnValue('0x' + 'b'.repeat(128)),
        signLookupMessage: vi.fn().mockResolvedValue('0x' + 'c'.repeat(762)),
      };
    }

    function mockServerLookupResponse(accountIds: string[]) {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          accounts: accountIds.map((id) => ({ account_id: id })),
        }),
      });
    }

    function mockServerStateResponse(accountId: string) {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: accountId,
          commitment: '0x' + 'f'.repeat(64),
          state_json: { data: 'base64data' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-01T00:00:00Z',
        }),
      });
    }

    it('returns one (accountId, state) pair when lookup matches a single account', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      const signer = makeLookupCapableSigner();
      const accountId = '0x7bfb0f38b0fafa103f86a805594170';

      mockServerLookupResponse([accountId]);
      mockServerStateResponse(accountId);

      const recovered = await client.recoverByKey(signer);

      expect(recovered).toHaveLength(1);
      expect(recovered[0].accountId).toBe(accountId);
      expect(recovered[0].state.commitment).toBe('0x' + 'f'.repeat(64));
      expect(signer.signLookupMessage).toHaveBeenCalledTimes(1);
      expect(signer.signLookupMessage).toHaveBeenCalledWith(
        signer.commitment,
        expect.any(Number)
      );
      // Lookup + getState = exactly two HTTP requests.
      expect(mockFetch).toHaveBeenCalledTimes(2);
    });

    it('returns multiple (accountId, state) pairs when one commitment authorizes several accounts', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      const signer = makeLookupCapableSigner();
      const accountA = '0xaaa1';
      const accountB = '0xbbb2';

      mockServerLookupResponse([accountA, accountB]);
      mockServerStateResponse(accountA);
      mockServerStateResponse(accountB);

      const recovered = await client.recoverByKey(signer);

      expect(recovered.map((r) => r.accountId)).toEqual([accountA, accountB]);
      // 1 lookup + 2 state fetches.
      expect(mockFetch).toHaveBeenCalledTimes(3);
    });

    it('returns empty array when no account authorizes the commitment', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      const signer = makeLookupCapableSigner();

      mockServerLookupResponse([]);

      const recovered = await client.recoverByKey(signer);

      expect(recovered).toEqual([]);
      // Only the lookup HTTP call — no per-account state fetches.
      expect(mockFetch).toHaveBeenCalledTimes(1);
    });

    it('throws a clear error when the signer does not implement signLookupMessage', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);
      // mockSigner from the outer beforeEach lacks signLookupMessage.
      await expect(client.recoverByKey(mockSigner)).rejects.toThrow(/signLookupMessage/);
      expect(mockFetch).not.toHaveBeenCalled();
    });
  });
});
