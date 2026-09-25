import { describe, it, expect, vi, beforeEach } from 'vitest';
import { MultisigClient } from './client.js';
import { readOnChainCommitment } from './state/adopt.js';
import type { Signer } from './types.js';

// Mock the Miden SDK
vi.mock('@miden-sdk/miden-sdk', () => ({
  AccountId: {
    fromHex: vi.fn((hex: string) => ({ toString: () => hex })),
  },
  Account: {
    // `nonce` and `to_commitment` are part of the shape now: `load` reconciles
    // the incoming account against the store and against chain, so a stub
    // without them is not an account as far as that path is concerned. Nonce
    // zero means never transacted, which is what these tests are about.
    deserialize: vi.fn(() => ({
      id: () => ({
        toString: () => '0x' + 'd'.repeat(30),
        prefix: () => ({ asInt: () => BigInt(1) }),
        suffix: () => ({ asInt: () => BigInt(2) }),
      }),
      nonce: () => ({ asInt: () => BigInt(0) }),
      to_commitment: () => ({ toHex: () => '0x' + 'e'.repeat(64) }),
      serialize: () => new Uint8Array([1, 2, 3]),
      storage: vi.fn(),
      vault: vi.fn(),
    })),
  },
  Word: vi.fn(),
}));

// Mock the AccountInspector, keeping the real assertCompleteDetectedConfig
// so load()'s fail-closed validation is exercised.
vi.mock('./inspector.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./inspector.js')>();
  return {
    ...actual,
    AccountInspector: {
      fromAccount: vi.fn(() => ({
        threshold: 2,
        numSigners: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
        vaultBalances: [],
        procedureThresholds: new Map(),
      })),
    },
  };
});

// Only the on-chain read is stubbed; isSafeToAdoptGuardianState stays real so
// the nonce and commitment rules are the ones under test.
vi.mock('./state/adopt.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./state/adopt.js')>();
  return {
    ...actual,
    readOnChainCommitment: vi.fn().mockResolvedValue(null),
  };
});

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
    // Reset per test, not just declared once. These are `mockResolvedValueOnce`
    // queues, so a test whose code path does not read the node leaves its value
    // behind for the next one, which then asserts against another test's setup.
    // That coupling made an unrelated change here fail a test three cases away.
    vi.mocked(readOnChainCommitment).mockReset().mockResolvedValue(null);

    webClient = {
      accounts: {
        get: vi.fn().mockResolvedValue(null),
        insert: vi.fn().mockResolvedValue(undefined),
      },
      keystore: {
        insert: vi.fn().mockResolvedValue(undefined),
      },
    };
    // `load` reads and writes the account through the raw client (so an adapter
    // can take them). This mock has no `sync`, so it is used as the raw client
    // itself; its raw methods delegate to `accounts` to keep the assertions below.
    webClient.getAccount = vi.fn((id: unknown) => webClient.accounts.get(id));
    webClient.newAccount = vi.fn((account: unknown, overwrite: boolean) =>
      webClient.accounts.insert({ account, overwrite }),
    );

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

    it('fails closed when the detected signer set is incomplete (issue #306 review)', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      const { AccountInspector } = await import('./inspector.js');
      // Storage reports 3 signers but only 2 entries were readable — adopting
      // this config would let membership proposals drop the missing key.
      vi.mocked(AccountInspector.fromAccount).mockReturnValueOnce({
        threshold: 2,
        numSigners: 3,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
        vaultBalances: [],
        procedureThresholds: new Map(),
      });

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

      await expect(client.load('0x' + 'd'.repeat(30), mockSigner)).rejects.toThrow(
        /incomplete signer set: storage reports 3 signers, read 2/,
      );
    });

    it('fails closed when the guardian commitment is missing', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      const { AccountInspector } = await import('./inspector.js');
      vi.mocked(AccountInspector.fromAccount).mockReturnValueOnce({
        threshold: 1,
        numSigners: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: null,
        vaultBalances: [],
        procedureThresholds: new Map(),
      });

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

      await expect(client.load('0x' + 'd'.repeat(30), mockSigner)).rejects.toThrow(
        /missing guardian commitment/,
      );
    });

    it('should throw if account not found on GUARDIAN', async () => {
      const client = new MultisigClient(webClient, CLIENT_CONFIG);

      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 404,
        statusText: 'Not Found',
        headers: new Headers(),
        text: async () =>
          JSON.stringify({
            code: 'account_not_found',
            message: 'Account not found',
            meta: { retryable: false },
          }),
      });

      await expect(
        client.load('0xnonexistent', mockSigner)
      ).rejects.toThrow('Account not found');
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

    // Writing GUARDIAN's account only when the store held nothing meant a caller
    // that already had the account read its own stale copy, while the returned
    // Multisig carried a config derived from GUARDIAN's.
    describe('reconciling the store with GUARDIAN', () => {
      const ACCOUNT_ID = '0x' + 'd'.repeat(30);
      const GUARDIAN_COMMITMENT = '0x' + '7'.repeat(64);
      const LOCAL_COMMITMENT = '0x' + '8'.repeat(64);

      function accountAt(nonce: bigint, commitment: string) {
        return {
          id: () => ({
            toString: () => ACCOUNT_ID,
            prefix: () => ({ asInt: () => BigInt(1) }),
            suffix: () => ({ asInt: () => BigInt(2) }),
          }),
          nonce: () => ({ asInt: () => nonce }),
          to_commitment: () => ({ toHex: () => commitment }),
          serialize: () => new Uint8Array([1, 2, 3]),
          storage: vi.fn(),
          vault: vi.fn(),
        };
      }

      async function stubGuardianAccount(account: unknown) {
        const { Account } = await import('@miden-sdk/miden-sdk');
        vi.mocked(Account.deserialize).mockReturnValueOnce(account as never);
        mockFetch.mockResolvedValueOnce({
          ok: true,
          json: async () => ({
            account_id: ACCOUNT_ID,
            commitment: GUARDIAN_COMMITMENT,
            state_json: { data: 'base64state' },
            created_at: '2024-01-01T00:00:00Z',
            updated_at: '2024-01-02T00:00:00Z',
          }),
        });
      }

      it('overwrites a store record GUARDIAN has moved past', async () => {
        const { readOnChainCommitment } = await import('./state/adopt.js');
        vi.mocked(readOnChainCommitment).mockResolvedValueOnce(GUARDIAN_COMMITMENT);

        const incoming = accountAt(BigInt(2), GUARDIAN_COMMITMENT);
        webClient.accounts.get.mockResolvedValueOnce(accountAt(BigInt(1), LOCAL_COMMITMENT));
        await stubGuardianAccount(incoming);

        const multisig = await new MultisigClient(webClient, CLIENT_CONFIG).load(
          ACCOUNT_ID,
          mockSigner,
        );

        expect(webClient.accounts.insert).toHaveBeenCalledWith({
          account: incoming,
          overwrite: true,
        });
        expect(multisig.account).toBe(incoming);
      });

      it('keeps a store record that is ahead of GUARDIAN, and describes that one', async () => {
        // Between pushing a delta and GUARDIAN canonicalizing it, local is
        // legitimately ahead; overwriting would build the next transaction on a
        // stale nonce.
        const local = accountAt(BigInt(3), LOCAL_COMMITMENT);
        webClient.accounts.get.mockResolvedValueOnce(local);
        await stubGuardianAccount(accountAt(BigInt(2), GUARDIAN_COMMITMENT));

        const multisig = await new MultisigClient(webClient, CLIENT_CONFIG).load(
          ACCOUNT_ID,
          mockSigner,
        );

        expect(webClient.accounts.insert).not.toHaveBeenCalled();
        expect(multisig.account).toBe(local);
      });

      it('refuses to adopt when a transacted account reads as undeployed', async () => {
        // `readOnChainCommitment` reports a missing account by matching `not
        // found` in the error text, which a proxy or gateway 404 also says. An
        // account with a non-zero nonce has transacted, so a null commitment
        // there is an RPC failure, and adopting on it would skip the check
        // against chain altogether.
        const { readOnChainCommitment } = await import('./state/adopt.js');
        vi.mocked(readOnChainCommitment).mockResolvedValueOnce(null);

        webClient.accounts.get.mockResolvedValueOnce(accountAt(BigInt(4), LOCAL_COMMITMENT));
        await stubGuardianAccount(accountAt(BigInt(5), GUARDIAN_COMMITMENT));

        await expect(
          new MultisigClient(webClient, CLIENT_CONFIG).load(ACCOUNT_ID, mockSigner),
        ).rejects.toThrow(/has transacted/);
        expect(webClient.accounts.insert).not.toHaveBeenCalled();
      });

      // The one shape a null on-chain commitment legitimately describes: an
      // account that has never transacted, so there is nothing on chain to
      // disagree with, loaded into a client that has never held it.
      it('still adopts an undeployed account into an empty store', async () => {
        const { readOnChainCommitment } = await import('./state/adopt.js');
        vi.mocked(readOnChainCommitment).mockResolvedValueOnce(null);

        const incoming = accountAt(BigInt(0), GUARDIAN_COMMITMENT);
        webClient.accounts.get.mockResolvedValueOnce(null);
        await stubGuardianAccount(incoming);

        const multisig = await new MultisigClient(webClient, CLIENT_CONFIG).load(
          ACCOUNT_ID,
          mockSigner,
        );
        expect(multisig.account).toBe(incoming);
        expect(webClient.accounts.insert).toHaveBeenCalled();
      });

      // An empty store is the ordinary shape for loading an account this client
      // has never held: a cosigner opening an account someone else created, or
      // any client on a fresh store. Without a check here that path would take
      // GUARDIAN's word with no reference to chain at all.
      it('checks against chain even when the store is empty', async () => {
        const { readOnChainCommitment } = await import('./state/adopt.js');
        vi.mocked(readOnChainCommitment).mockResolvedValueOnce(null);

        webClient.accounts.get.mockResolvedValueOnce(null);
        await stubGuardianAccount(accountAt(BigInt(7), GUARDIAN_COMMITMENT));

        await expect(
          new MultisigClient(webClient, CLIENT_CONFIG).load(ACCOUNT_ID, mockSigner),
        ).rejects.toThrow(/has transacted/);
        expect(webClient.accounts.insert).not.toHaveBeenCalled();
      });

      // Checking that *some* commitment exists on chain is not checking against
      // chain. Without the comparison, a fresh cosigner adopts whatever GUARDIAN
      // serves as long as the account is deployed at all, which is the case this
      // guard exists for.
      it('refuses incoming state that disagrees with the on-chain commitment on an empty store', async () => {
        const { readOnChainCommitment } = await import('./state/adopt.js');
        vi.mocked(readOnChainCommitment).mockResolvedValueOnce(LOCAL_COMMITMENT);

        webClient.accounts.get.mockResolvedValueOnce(null);
        await stubGuardianAccount(accountAt(BigInt(7), GUARDIAN_COMMITMENT));

        await expect(
          new MultisigClient(webClient, CLIENT_CONFIG).load(ACCOUNT_ID, mockSigner),
        ).rejects.toThrow(/does not match on-chain commitment/);
        expect(webClient.accounts.insert).not.toHaveBeenCalled();
      });

      it('leaves an already-matching store record alone rather than reading it as divergence', async () => {
        // Equal nonce with differing commitments is divergence and throws, so a
        // pair that already agrees must never reach that rule.
        const local = accountAt(BigInt(2), GUARDIAN_COMMITMENT);
        webClient.accounts.get.mockResolvedValueOnce(local);
        await stubGuardianAccount(accountAt(BigInt(2), GUARDIAN_COMMITMENT));

        const multisig = await new MultisigClient(webClient, CLIENT_CONFIG).load(
          ACCOUNT_ID,
          mockSigner,
        );

        expect(webClient.accounts.insert).not.toHaveBeenCalled();
        expect(multisig.account).toBe(local);
      });
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
