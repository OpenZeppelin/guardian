import { describe, it, expect, vi, beforeEach } from 'vitest';
import { MultisigClient } from './client.js';
import type { Signer } from './types.js';

// Mock the Miden SDK
vi.mock('@demox-labs/miden-sdk', () => ({
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
      psmEnabled: true,
      psmCommitment: '0x' + 'c'.repeat(64),
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

// Mock fetch for PSM client
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

describe('MultisigClient', () => {
  let webClient: any;
  let mockSigner: Signer;

  beforeEach(() => {
    mockFetch.mockReset();

    webClient = {
      createScriptBuilder: vi.fn(),
      executeTransaction: vi.fn(),
      proveTransaction: vi.fn(),
      submitProvenTransaction: vi.fn(),
      applyTransaction: vi.fn(),
      syncState: vi.fn(),
      newAccount: vi.fn(),
      getAccount: vi.fn().mockResolvedValue(null),
    };

    mockSigner = {
      scheme: 'falcon',
      commitment: '0x' + '1'.repeat(64),
      publicKey: '0x' + '2'.repeat(64),
      signAccountIdWithTimestamp: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
      signCommitment: vi.fn().mockReturnValue('0x' + 'b'.repeat(128)),
    };
  });

  describe('constructor', () => {
    it('should create client with default PSM endpoint', () => {
      const client = new MultisigClient(webClient);
      expect(client).toBeInstanceOf(MultisigClient);
    });

    it('should create client with custom PSM endpoint', () => {
      const client = new MultisigClient(webClient, { psmEndpoint: 'http://custom:8080' });
      expect(client).toBeInstanceOf(MultisigClient);
    });
  });

  describe('psmClient getter', () => {
    it('should expose PSM client for getting pubkey', () => {
      const client = new MultisigClient(webClient);
      expect(client.psmClient).toBeDefined();
    });
  });

  describe('create', () => {
    it('should create multisig and return Multisig instance', async () => {
      const client = new MultisigClient(webClient);

      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = await client.create(config, mockSigner);

      expect(multisig).toBeDefined();
      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual(config.signerCommitments);
      expect(multisig.psmCommitment).toBe(config.psmCommitment);
    });

    it('should set signer on PSM client', async () => {
      const client = new MultisigClient(webClient);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = await client.create(config, mockSigner);
      expect(multisig.signerCommitment).toBe(mockSigner.commitment);
    });
  });

  describe('load', () => {
    it('should load existing multisig account and detect config', async () => {
      const client = new MultisigClient(webClient);

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
      expect(multisig.psmCommitment).toBe('0x' + 'c'.repeat(64));
      expect(multisig.account).toBeNull(); // Loaded accounts don't have the SDK Account
    });

    it('should throw if account not found on PSM', async () => {
      const client = new MultisigClient(webClient);

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
  });

  describe('initialize', () => {
    it('should fetch PSM pubkey and return commitment', async () => {
      const client = new MultisigClient(webClient);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          commitment: '0x' + 'f'.repeat(64),
          pubkey: '0x' + 'e'.repeat(64),
        }),
      });

      const result = await client.initialize('falcon');

      expect(result.psmCommitment).toBe('0x' + 'f'.repeat(64));
      expect(result.psmPublicKey).toBe('0x' + 'e'.repeat(64));
    });

    it('should work without specifying scheme', async () => {
      const client = new MultisigClient(webClient);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          commitment: '0x' + 'f'.repeat(64),
        }),
      });

      const result = await client.initialize();

      expect(result.psmCommitment).toBe('0x' + 'f'.repeat(64));
      expect(result.psmPublicKey).toBeUndefined();
    });
  });
});
