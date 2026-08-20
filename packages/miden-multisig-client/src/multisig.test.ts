import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Multisig } from './multisig.js';
import { GuardianHttpClient, type Signer } from '@openzeppelin/guardian-client';
import {
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateGuardianTransactionRequest,
  buildUpdateSignersTransactionRequest,
  executeForSummary,
} from './transaction.js';

const { mockRpcGetAccountDetails, mockAccountDeserialize, mockDetectConfig, mockNoteFileDeserialize } = vi.hoisted(() => ({
  mockRpcGetAccountDetails: vi.fn(),
  mockAccountDeserialize: vi.fn(),
  mockDetectConfig: vi.fn(),
  mockNoteFileDeserialize: vi.fn(),
}));

// Mock the Miden SDK
vi.mock('@miden-sdk/miden-sdk', () => ({
  Account: {
    deserialize: mockAccountDeserialize,
  },
  AccountId: {
    fromHex: vi.fn((hex: string) => ({ toString: () => hex })),
  },
  NoteType: {
    Private: 0,
    Public: 1,
  },
  NoteExportFormat: {
    Id: 0,
    Full: 1,
    Details: 2,
  },
  NoteFile: {
    deserialize: mockNoteFileDeserialize,
  },
  TransactionSummary: {
    deserialize: vi.fn().mockReturnValue({
      toCommitment: () => ({
        toHex: () => '0x' + 'c'.repeat(64),
      }),
      salt: () => ({
        toHex: () => '0x' + 'd'.repeat(64),
      }),
      serialize: () => new Uint8Array([1, 2, 3]),
    }),
  },
  Word: {
    fromHex: vi.fn((hex: string) => ({
      toHex: () => hex,
      toFelts: () => [1, 2, 3, 4],
    })),
  },
  Signature: {
    deserialize: vi.fn().mockReturnValue({
      toPreparedSignature: () => [1, 2, 3],
    }),
  },
  TransactionRequest: {
    deserialize: vi.fn().mockReturnValue({}),
  },
  AdviceMap: vi.fn().mockImplementation(() => ({
    insert: vi.fn(),
  })),
  FeltArray: vi.fn().mockImplementation((arr: any[]) => arr),
  Poseidon2: {
    hashElements: vi.fn().mockReturnValue({
      toHex: () => '0x' + 'e'.repeat(64),
    }),
  },
  Endpoint: vi.fn().mockImplementation((url: string) => ({ url })),
  RpcClient: vi.fn().mockImplementation(() => ({
    getAccountDetails: mockRpcGetAccountDetails,
  })),
}));

// Mock transaction module
vi.mock('./transaction.js', () => ({
  executeForSummary: vi.fn(),
  buildUpdateSignersTransactionRequest: vi.fn().mockResolvedValue({
    request: {},
    salt: { toHex: () => '0x' + 'd'.repeat(64) },
    configHash: { toHex: () => '0x' + 'e'.repeat(64) },
  }),
  buildUpdateProcedureThresholdTransactionRequest: vi.fn().mockResolvedValue({
    request: {},
    salt: { toHex: () => '0x' + 'd'.repeat(64) },
    configHash: { toHex: () => '0x' + 'e'.repeat(64) },
  }),
  buildUpdateGuardianTransactionRequest: vi.fn().mockResolvedValue({
    request: {},
    salt: { toHex: () => '0x' + 'd'.repeat(64) },
  }),
  buildConsumeNotesTransactionRequest: vi.fn().mockReturnValue({
    request: {},
    salt: { toHex: () => '0x' + 'd'.repeat(64) },
  }),
  buildP2idTransactionRequest: vi.fn().mockReturnValue({
    request: {},
    salt: { toHex: () => '0x' + 'd'.repeat(64) },
  }),
  buildP2idNoteFromMetadata: vi.fn().mockReturnValue({
    id: () => ({ toString: () => '0x' + 'ab'.repeat(32) }),
  }),
  // Mirrors the real implementations against the mocked NoteType values
  // (Private = 0, Public = 1).
  parseP2idNoteType: vi.fn((value?: string) => {
    if (value === undefined || value === 'public') return 1;
    if (value === 'private') return 0;
    throw new Error(`unsupported metadata.noteType '${value}': expected 'public' or 'private'`);
  }),
  p2idNoteTypeToMetadata: vi.fn((noteType?: number) => (noteType === 0 ? 'private' : undefined)),
}));

vi.mock('./utils/signature.js', async () => {
  const actual = await vi.importActual<typeof import('./utils/signature.js')>('./utils/signature.js');
  return {
    ...actual,
    buildSignatureAdviceEntry: vi.fn().mockImplementation((signerCommitment: { toHex?: () => string }) => ({
      key: { toHex: () => signerCommitment.toHex ? signerCommitment.toHex() : '0x' + 'f'.repeat(64) },
      values: [1, 2, 3],
    })),
    signatureHexToBytes: vi.fn((hex: string) => new Uint8Array([0, 1, 2, 3])),
  };
});

vi.mock('./utils/encoding.js', async () => {
  const actual = await vi.importActual<typeof import('./utils/encoding.js')>('./utils/encoding.js');
  return {
    ...actual,
    normalizeHexWord: vi.fn((hex: string) => '0x' + hex.replace(/^0x/i, '').toLowerCase().padStart(64, '0')),
  };
});

vi.mock('./inspector.js', () => ({
  AccountInspector: {
    fromAccount: mockDetectConfig,
  },
}));

// Mock fetch for GUARDIAN client
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

const MIDEN_RPC_ENDPOINT = 'https://rpc.devnet.miden.io';

function mockedAccount(commitmentHex: string, nonce = 0): any {
  return {
    commitment: () => ({
      toHex: () => commitmentHex,
    }),
    to_commitment: () => ({
      toHex: () => commitmentHex,
    }),
    nonce: () => ({
      asInt: () => BigInt(nonce),
    }),
  };
}

describe('Multisig', () => {
  let guardian: GuardianHttpClient;
  let mockSigner: Signer;
  let mockAccount: any;
  let mockWebClient: any;

  function createTestMultisig(
    config: ConstructorParameters<typeof Multisig>[1],
    signer: Signer = mockSigner,
    accountId?: string,
    proverConfig?: ConstructorParameters<typeof Multisig>[7],
  ): Multisig {
    return new Multisig(
      mockAccount,
      config,
      guardian,
      signer,
      mockWebClient,
      accountId,
      MIDEN_RPC_ENDPOINT,
      proverConfig,
    );
  }

  beforeEach(() => {
    mockFetch.mockReset();
    vi.mocked(executeForSummary).mockResolvedValue({
      toCommitment: () => ({
        toHex: () => '0x' + 'c'.repeat(64),
      }),
      serialize: () => new Uint8Array([1, 2, 3]),
    } as any);
    mockRpcGetAccountDetails.mockReset();
    mockAccountDeserialize.mockReset();
    mockRpcGetAccountDetails.mockResolvedValue({
      commitment: () => ({
        toHex: () => '0x' + 'b'.repeat(64),
      }),
    });
    mockAccountDeserialize.mockReturnValue(mockedAccount('0x' + 'b'.repeat(64), 1));
    mockDetectConfig.mockReset();
    mockDetectConfig.mockReturnValue({
      threshold: 1,
      numSigners: 1,
      signerCommitments: ['0x' + 'a'.repeat(64)],
      guardianEnabled: true,
      guardianCommitment: '0x' + 'c'.repeat(64),
      vaultBalances: [],
      procedureThresholds: new Map(),
    });

    guardian = new GuardianHttpClient('http://localhost:3000');

    mockSigner = {
      commitment: '0x' + '1'.repeat(64),
      publicKey: '0x' + '2'.repeat(64),
      scheme: 'falcon',
      signAccountIdWithTimestamp: vi.fn().mockResolvedValue('0x' + 'a'.repeat(128)),
      signRequest: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
      signCommitment: vi.fn().mockReturnValue('0x' + 'b'.repeat(128)),
    };

    guardian.setSigner(mockSigner);

    mockAccount = {
      id: () => ({
        toString: () => '0x' + 'a'.repeat(30),
        prefix: () => ({ asInt: () => BigInt(1) }),
        suffix: () => ({ asInt: () => BigInt(2) }),
      }),
      serialize: () => new Uint8Array([1, 2, 3]),
    };

    mockWebClient = {
      executeTransaction: vi.fn(),
      proveTransaction: vi.fn(),
      submitProvenTransaction: vi.fn(),
      applyTransaction: vi.fn(),
      submitNewTransaction: vi.fn(),
      submitNewTransactionWithProver: vi.fn(),
      transactions: {
        executeRequest: vi.fn(),
      },
      getConsumableNotes: vi.fn().mockResolvedValue([]),
      syncState: vi.fn(),
      getAccount: vi.fn().mockResolvedValue(null),
      newAccount: vi.fn(),
    };
    mockWebClient.transactions.executeRequest.mockImplementation(
      async (accountId: unknown, request: unknown) => {
        const result = await mockWebClient.executeTransaction(accountId, request);
        return {
          result,
          prove: async (options?: { prover?: unknown }) => {
            const proof = options?.prover === undefined
              ? await mockWebClient.proveTransaction(result)
              : await mockWebClient.proveTransaction(result, options.prover);
            return {
              proof,
              result,
              submit: async () => {
                const blockNumber = await mockWebClient.submitProvenTransaction(proof, result);
                return {
                  blockNumber,
                  result,
                  apply: () => mockWebClient.applyTransaction(result, blockNumber),
                };
              },
            };
          },
        };
      },
    );
  });

  describe('constructor', () => {
    it('should create Multisig with account', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual(config.signerCommitments);
      expect(multisig.guardianCommitment).toBe(config.guardianCommitment);
      expect(multisig.account).toBe(mockAccount);
    });

    it('should create Multisig with explicit accountId override', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const accountId = '0x' + 'd'.repeat(30);
      const multisig = createTestMultisig(config, mockSigner, accountId);

      expect(multisig.account).toBe(mockAccount);
      expect(multisig.accountId).toBe(accountId);
    });

    it('should reject a missing Miden RPC endpoint immediately', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      expect(
        () => new Multisig(
          mockAccount,
          config,
          guardian,
          mockSigner,
          mockWebClient,
          undefined,
          undefined as unknown as string
        )
      ).toThrow('missing required configuration: midenRpcEndpoint');
    });
  });

  describe('accountId', () => {
    it('should return account ID from account', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      expect(multisig.accountId).toBe('0x' + 'a'.repeat(30));
    });

    it('should return provided account ID when constructor override is set', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const accountId = '0x' + 'e'.repeat(30);
      const multisig = createTestMultisig(config, mockSigner, accountId);
      expect(multisig.accountId).toBe(accountId);
    });
  });

  describe('history (issue #413)', () => {
    it('delegates to the guardian client with the account id and options', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      const multisig = createTestMultisig(config);

      const page = {
        entries: [
          {
            nonce: 3,
            timestamp: '2026-08-01T12:00:03Z',
            newCommitment: '0x' + 'b'.repeat(64),
            inputNotes: [],
            outputNotes: [],
            decodeWarnings: [],
          },
        ],
        nextCursor: 'cursor-token',
      };
      const spy = vi.spyOn(guardian, 'getTransactionHistory').mockResolvedValue(page);

      const result = await multisig.transactionHistory({ limit: 5, cursor: 'prev' });

      expect(result).toBe(page);
      expect(spy).toHaveBeenCalledWith('0x' + 'a'.repeat(30), { limit: 5, cursor: 'prev' });
    });
  });

  describe('signerCommitment', () => {
    it('should return signer commitment', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      expect(multisig.signerCommitment).toBe(mockSigner.commitment);
    });
  });

  describe('fetchState', () => {
    it('should fetch account state from GUARDIAN', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: '0x' + 'a'.repeat(30),
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'base64state' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      const state = await multisig.fetchState();

      expect(state.accountId).toBe('0x' + 'a'.repeat(30));
      expect(state.commitment).toBe('0x' + 'b'.repeat(64));
      expect(state.stateDataBase64).toBe('base64state');
    });
  });

  describe('syncState', () => {
    it('should overwrite local state when account is missing locally', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce(null);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      await multisig.syncState();

      expect(mockWebClient.newAccount).toHaveBeenCalledTimes(1);
      expect(mockRpcGetAccountDetails).toHaveBeenCalledTimes(1);
    });

    it('should overwrite local state when incoming commitment matches on-chain commitment', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'a'.repeat(64), 0));
      mockRpcGetAccountDetails.mockResolvedValueOnce({
        commitment: () => ({
          toHex: () => '0x' + 'b'.repeat(64),
        }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      await multisig.syncState();

      expect(mockWebClient.newAccount).toHaveBeenCalledTimes(1);
    });

    it('refreshes multisig config from synced account state', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'b'.repeat(64), 0));
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });
      mockDetectConfig.mockReturnValueOnce({
        threshold: 2,
        numSigners: 2,
        signerCommitments: ['0x' + '1'.repeat(64), '0x' + '2'.repeat(64)],
        guardianEnabled: true,
        guardianCommitment: '0x' + 'd'.repeat(64),
        vaultBalances: [],
        procedureThresholds: new Map(),
      });

      await multisig.syncState();

      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual([
        '0x' + '1'.repeat(64),
        '0x' + '2'.repeat(64),
      ]);
      expect(multisig.guardianCommitment).toBe('0x' + 'd'.repeat(64));
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
    });

    it('should overwrite local state when account is not found on-chain', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'a'.repeat(64), 0));
      mockRpcGetAccountDetails.mockRejectedValueOnce(
        new Error('No account header record found for given ID')
      );
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      await multisig.syncState();

      expect(mockWebClient.newAccount).toHaveBeenCalledTimes(1);
    });

    it('should throw when incoming commitment does not match on-chain commitment', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'a'.repeat(64), 0));
      mockAccountDeserialize.mockReturnValueOnce(mockedAccount('0x' + 'b'.repeat(64), 1));
      mockRpcGetAccountDetails.mockResolvedValueOnce({
        commitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      await expect(multisig.syncState()).rejects.toThrow('Refusing to overwrite local state');
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
    });

    it('keeps local state and refreshes config from it when GUARDIAN nonce is behind local', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      const localAccount = mockedAccount('0x' + 'a'.repeat(64), 3);
      mockWebClient.getAccount.mockResolvedValueOnce(localAccount);
      mockAccountDeserialize.mockReturnValueOnce(mockedAccount('0x' + 'b'.repeat(64), 2));
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });
      mockDetectConfig.mockReturnValueOnce({
        threshold: 2,
        numSigners: 2,
        signerCommitments: ['0x' + '1'.repeat(64), '0x' + '2'.repeat(64)],
        guardianEnabled: true,
        guardianCommitment: '0x' + 'd'.repeat(64),
        vaultBalances: [],
        procedureThresholds: new Map(),
      });

      // GUARDIAN behind local (nonce 2 < 3): no throw, local kept, no overwrite,
      // and the decision needs no on-chain round-trip.
      await expect(multisig.syncState()).resolves.toBeDefined();
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
      expect(mockRpcGetAccountDetails).not.toHaveBeenCalled();
      // Config refreshed from the authoritative local account (UI unfreezes).
      expect(multisig.account).toBe(localAccount);
      expect(multisig.threshold).toBe(2);
    });

    it('should throw when incoming state nonce equals local nonce but commitment differs', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'a'.repeat(64), 2));
      mockAccountDeserialize.mockReturnValueOnce(mockedAccount('0x' + 'b'.repeat(64), 2));
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      await expect(multisig.syncState()).rejects.toThrow(
        'incoming nonce 2 equals local nonce 2 but commitments differ'
      );
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
    });

    it('unfreezes Multisig.account after execute when GUARDIAN still lags by one nonce (regression, #343)', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      // Post-execute: local advanced to nonce 1, GUARDIAN still reports nonce 0
      // (candidate not canonicalized yet). Before the fix this threw and left
      // Multisig.account frozen at the pre-execute snapshot.
      const localAccount = mockedAccount('0x' + 'a'.repeat(64), 1);
      mockWebClient.getAccount.mockResolvedValueOnce(localAccount);
      mockAccountDeserialize.mockReturnValueOnce(mockedAccount('0x' + 'b'.repeat(64), 0));
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          commitment: '0x' + 'b'.repeat(64),
          state_json: { data: 'AQID' },
          created_at: '2024-01-01T00:00:00Z',
          updated_at: '2024-01-02T00:00:00Z',
        }),
      });

      await expect(multisig.syncState()).resolves.toBeDefined();
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
      expect(multisig.account).toBe(localAccount);
    });
  });

  describe('verifyStateCommitment', () => {
    it('should pass when local and on-chain commitments match', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'b'.repeat(64), 0));

      const multisigWithRpc = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      await expect(
        multisigWithRpc.verifyStateCommitment()
      ).resolves.toMatchObject({
        accountId: multisigWithRpc.accountId,
      });
    });

    it('should throw when local account state is missing', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      mockWebClient.getAccount.mockResolvedValueOnce(null);

      const multisigWithRpc = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      await expect(
        multisigWithRpc.verifyStateCommitment()
      ).rejects.toThrow('Local account state not found');
    });

    it('should throw when local and on-chain commitments differ', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'f'.repeat(64), 0));
      mockRpcGetAccountDetails.mockResolvedValueOnce({
        commitment: () => ({
          toHex: () => '0x' + 'b'.repeat(64),
        }),
      });

      const multisigWithRpc = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      await expect(
        multisigWithRpc.verifyStateCommitment()
      ).rejects.toThrow('Local account commitment does not match on-chain commitment');
    });
  });

  describe('registerOnGuardian', () => {
    it('should register account on GUARDIAN', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: true,
          message: 'Account configured',
          ack_pubkey: '0x' + 'd'.repeat(64),
        }),
      });

      await expect(multisig.registerOnGuardian()).resolves.toBeUndefined();
    });

    it('should register ECDSA accounts with MidenEcdsa auth', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const ecdsaSigner: Signer = {
        ...mockSigner,
        publicKey: '0x' + '2'.repeat(66),
        scheme: 'ecdsa',
      };

      guardian.setSigner(ecdsaSigner);
      const multisig = createTestMultisig(config, ecdsaSigner);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: true,
          message: 'Account configured',
          ack_pubkey: '0x' + 'd'.repeat(66),
        }),
      });

      await expect(multisig.registerOnGuardian()).resolves.toBeUndefined();

      const [, requestInit] = mockFetch.mock.calls[0] as [string, RequestInit];
      const body = JSON.parse(String(requestInit.body));
      expect(body.auth).toEqual({
        MidenEcdsa: {
          cosigner_commitments: config.signerCommitments,
        },
      });
    });

    it('should accept explicit initial state base64', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        guardian,
        mockSigner,
        mockWebClient,
        '0x' + 'e'.repeat(30),
        MIDEN_RPC_ENDPOINT,
      );

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: true,
          message: 'Account configured',
        }),
      });

      await expect(multisig.registerOnGuardian('base64initialstate')).resolves.toBeUndefined();
    });

    it('should throw on GUARDIAN registration failure', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: false,
          message: 'Account already exists',
        }),
      });

      await expect(multisig.registerOnGuardian()).rejects.toThrow('Failed to register on GUARDIAN');
    });
  });

  describe('syncProposals', () => {
    it('should sync proposals from GUARDIAN', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
          metadata: {
            proposal_type: 'add_signer',
            target_threshold: 1,
            signer_commitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposals }),
      });

      const proposals = await multisig.syncProposals();

      expect(proposals.length).toBe(1);
      expect(proposals[0].nonce).toBe(1);
      expect(proposals[0].status).toBe('pending');
    });

    it('should return ready status when enough signatures', async () => {
      const config = {
        threshold: 1, // Only 1 signature needed
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
          metadata: {
            proposal_type: 'add_signer',
            target_threshold: 1,
            signer_commitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposals }),
      });

      const proposals = await multisig.syncProposals();

      expect(proposals[0].status).toBe('ready');
    });

    it('should reject proposals whose metadata does not match tx_summary', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          proposals: [
            {
              account_id: '0x' + 'a'.repeat(30),
              nonce: 1,
              prev_commitment: '0x' + 'b'.repeat(64),
              delta_payload: {
                tx_summary: { data: 'AQID' },
                signatures: [],
                metadata: {
                  proposal_type: 'add_signer',
                  target_threshold: 1,
                  signer_commitments: ['0x' + 'a'.repeat(64)],
                  description: '',
                },
              },
              status: {
                status: 'pending',
                timestamp: '2024-01-01T00:00:00Z',
                proposer_id: '0x' + 'c'.repeat(64),
                cosigner_sigs: [],
              },
            },
          ],
        }),
      });

      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      } as any);

      await expect(multisig.syncProposals()).rejects.toThrow(
        'Invalid proposal: metadata does not match tx_summary'
      );
    });

    it('should reject non-32-byte signer IDs from GUARDIAN proposals', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'add_signer',
              target_threshold: 1,
              signer_commitments: ['0x' + 'a'.repeat(64)],
              description: '',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x1',
                signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposals }),
      });

      await expect(multisig.syncProposals()).rejects.toThrow('expected signerId as 32-byte hex');
    });

    it('should reject duplicate normalized signer IDs from GUARDIAN proposals', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'add_signer',
              target_threshold: 2,
              signer_commitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
              description: '',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'A'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
                timestamp: '2024-01-01T00:00:00Z',
              },
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'f'.repeat(128) },
                timestamp: '2024-01-01T00:00:01Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposals }),
      });

      await expect(multisig.syncProposals()).rejects.toThrow('duplicate signatures for signer');
    });
  });

  describe('listProposals', () => {
    it('should return empty list initially', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      expect(multisig.listProposals()).toEqual([]);
    });
  });

  describe('createProposal', () => {
    it('should create a new proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createProposal(1, 'AQID', {
        proposalType: 'add_signer',
        targetThreshold: 1,
        targetSignerCommitments: ['0x' + 'a'.repeat(64)],
        description: '',
      });

      expect(proposal.nonce).toBe(1);
      expect(proposal.id).toBe('0x' + 'c'.repeat(64));
    });

    it('should reject a mismatched returned commitment', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'd'.repeat(64),
        }),
      });

      await expect(
        multisig.createProposal(1, 'AQID', {
          proposalType: 'add_signer',
          targetThreshold: 1,
          targetSignerCommitments: ['0x' + 'a'.repeat(64)],
          description: '',
        }),
      ).rejects.toThrow(
        'Invalid proposal: commitment 0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd does not match tx_summary 0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc',
      );
    });

    it('should reject a response whose tx_summary does not match the provided metadata', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      } as any);

      await expect(
        multisig.createProposal(1, 'AQID', {
          proposalType: 'add_signer',
          targetThreshold: 1,
          targetSignerCommitments: ['0x' + 'a'.repeat(64)],
          description: '',
        })
      ).rejects.toThrow('Invalid proposal: metadata does not match tx_summary');
    });
  });

  describe('createP2idProposal', () => {
    it('should include the faucet asset in the proposal description', async () => {
      const { executeForSummary } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        toCommitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'p2id',
            recipient_id: '0xrecipient',
            faucet_id: '0xfaucet',
            amount: '100',
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createP2idProposal('0xrecipient', '0xfaucet', 100n, 1);

      expect(proposal.metadata.description).toBe('Send 100 of asset 0xfaucet... to 0xrecipien...');
    });

    it('threads a private noteType into the request and wire metadata (issue #322)', async () => {
      const { executeForSummary, buildP2idTransactionRequest } = await import('./transaction.js');
      const { NoteType } = await import('@miden-sdk/miden-sdk');
      vi.mocked(executeForSummary).mockResolvedValue({
        toCommitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'p2id',
            recipient_id: '0xrecipient',
            faucet_id: '0xfaucet',
            amount: '100',
            note_type: 'private',
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createP2idProposal('0xrecipient', '0xfaucet', 100n, 1, {
        noteType: NoteType.Private,
      });

      // Propose path builds the private note...
      expect(vi.mocked(buildP2idTransactionRequest)).toHaveBeenCalledWith(
        expect.any(String),
        '0xrecipient',
        '0xfaucet',
        100n,
        expect.anything(),
        { noteType: NoteType.Private },
      );
      // ...and the rebuild-from-metadata path parses note_type back to Private.
      const lastCall = vi.mocked(buildP2idTransactionRequest).mock.calls.at(-1)!;
      expect(lastCall[5]).toMatchObject({ noteType: NoteType.Private });

      // The pushed wire metadata carries note_type so cosigners rebuild the
      // same private note at verification/execution.
      const pushBody = JSON.parse(mockFetch.mock.calls.at(-1)![1].body as string);
      expect(pushBody.delta_payload.metadata.note_type).toBe('private');

      expect(proposal.metadata.proposalType).toBe('p2id');
      expect((proposal.metadata as { noteType?: string }).noteType).toBe('private');
    });
  });

  describe('exportNoteToBytes / importNoteFromBytes (issue #356)', () => {
    const config = {
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      guardianCommitment: '0x' + '3'.repeat(64),
    };

    it('exports the full note with proof when the inclusion proof is known', async () => {
      const noteFile = { serialize: () => new Uint8Array([9, 9, 9]) };
      mockWebClient.getOutputNote = vi.fn().mockResolvedValue({
        inclusionProof: () => ({}),
      });
      mockWebClient.exportNoteFile = vi.fn().mockResolvedValue(noteFile);

      const multisig = createTestMultisig(config);
      const bytes = await multisig.exportNoteToBytes('0x' + 'ab'.repeat(32));

      expect(bytes).toEqual(new Uint8Array([9, 9, 9]));
      // NoteExportFormat.Full = 1 in the SDK mock
      expect(mockWebClient.exportNoteFile).toHaveBeenCalledWith('0x' + 'ab'.repeat(32), 1);
    });

    it('falls back to a details-only export before the note commits on chain', async () => {
      const noteFile = { serialize: () => new Uint8Array([7]) };
      mockWebClient.getOutputNote = vi.fn().mockResolvedValue({
        inclusionProof: () => undefined,
      });
      mockWebClient.exportNoteFile = vi.fn().mockResolvedValue(noteFile);

      const multisig = createTestMultisig(config);
      await multisig.exportNoteToBytes(' 0x' + 'ab'.repeat(32) + ' ');

      // NoteExportFormat.Details = 2 in the SDK mock; the id is trimmed
      expect(mockWebClient.exportNoteFile).toHaveBeenCalledWith('0x' + 'ab'.repeat(32), 2);
    });

    it('rejects exporting a note the local store does not know', async () => {
      mockWebClient.getOutputNote = vi.fn().mockRejectedValue(new Error('no such note'));
      mockWebClient.exportNoteFile = vi.fn();

      const multisig = createTestMultisig(config);
      await expect(multisig.exportNoteToBytes('0x' + 'ab'.repeat(32))).rejects.toThrow(
        /not found in the local store/,
      );
      expect(mockWebClient.exportNoteFile).not.toHaveBeenCalled();
    });

    it('rejects exporting when the store resolves no record', async () => {
      mockWebClient.getOutputNote = vi.fn().mockResolvedValue(undefined);
      mockWebClient.exportNoteFile = vi.fn();

      const multisig = createTestMultisig(config);
      await expect(multisig.exportNoteToBytes('0x' + 'ab'.repeat(32))).rejects.toThrow(
        /not found in the local store/,
      );
      expect(mockWebClient.exportNoteFile).not.toHaveBeenCalled();
    });

    it('imports note file bytes and returns the resolved identifier', async () => {
      const decoded = { marker: 'note-file' };
      mockNoteFileDeserialize.mockReturnValue(decoded);
      mockWebClient.importNoteFile = vi.fn().mockResolvedValue('0x' + 'cd'.repeat(32));

      const multisig = createTestMultisig(config);
      const noteId = await multisig.importNoteFromBytes(new Uint8Array([1, 2, 3]));

      expect(mockNoteFileDeserialize).toHaveBeenCalledWith(new Uint8Array([1, 2, 3]));
      expect(mockWebClient.importNoteFile).toHaveBeenCalledWith(decoded);
      expect(noteId).toBe('0x' + 'cd'.repeat(32));
    });

    it('rejects bytes that do not decode as a note file', async () => {
      mockNoteFileDeserialize.mockImplementation(() => {
        throw new Error('bad bytes');
      });
      mockWebClient.importNoteFile = vi.fn();

      const multisig = createTestMultisig(config);
      await expect(multisig.importNoteFromBytes(new Uint8Array([0]))).rejects.toThrow(
        /failed to decode note file: bad bytes/,
      );
      expect(mockWebClient.importNoteFile).not.toHaveBeenCalled();
    });
  });

  describe('exportNoteToFile / importNoteFromFile (issue #356)', () => {
    const config = {
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      guardianCommitment: '0x' + '3'.repeat(64),
    };

    it('rejects exportNoteToFile outside a browser environment', async () => {
      const multisig = createTestMultisig(config);
      await expect(multisig.exportNoteToFile('0x' + 'ab'.repeat(32))).rejects.toThrow(
        /requires a browser environment/,
      );
    });

    it('imports from a File/Blob by delegating to importNoteFromBytes', async () => {
      const decoded = { marker: 'note-file' };
      mockNoteFileDeserialize.mockReturnValue(decoded);
      mockWebClient.importNoteFile = vi.fn().mockResolvedValue('0x' + 'cd'.repeat(32));

      const multisig = createTestMultisig(config);
      const noteId = await multisig.importNoteFromFile(new Blob([new Uint8Array([1, 2, 3])]));

      expect(mockNoteFileDeserialize).toHaveBeenCalledWith(new Uint8Array([1, 2, 3]));
      expect(noteId).toBe('0x' + 'cd'.repeat(32));
    });
  });

  describe('getP2idNoteId (issue #356)', () => {
    const config = {
      threshold: 1,
      signerCommitments: ['0x' + '1'.repeat(64)],
      guardianCommitment: '0x' + '3'.repeat(64),
    };

    it('computes the deterministic note ID from p2id proposal metadata', async () => {
      const multisig = createTestMultisig(config);
      const proposal = {
        metadata: {
          proposalType: 'p2id',
          recipientId: '0x' + 'b'.repeat(30),
          faucetId: '0x' + 'c'.repeat(30),
          amount: '100',
          saltHex: '0x' + 'd'.repeat(64),
          noteType: 'private',
        },
      } as any;

      const noteId = await multisig.getP2idNoteId(proposal);
      expect(noteId).toBe('0x' + 'ab'.repeat(32));
    });

    it('rejects non-p2id proposals', async () => {
      const multisig = createTestMultisig(config);
      const proposal = {
        metadata: { proposalType: 'consume_notes' },
      } as any;

      await expect(multisig.getP2idNoteId(proposal)).rejects.toThrow(
        /requires a P2ID proposal/,
      );
    });
  });

  describe('createChangeThresholdProposal', () => {
    it('passes the signer scheme to update-signers requests', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        toCommitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const ecdsaSigner: Signer = {
        ...mockSigner,
        publicKey: '0x' + '2'.repeat(66),
        scheme: 'ecdsa',
      };
      guardian.setSigner(ecdsaSigner);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'change_threshold',
            target_threshold: 2,
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const multisig = createTestMultisig(config, ecdsaSigner);
      await multisig.createChangeThresholdProposal(2, 1);

      expect(buildUpdateSignersTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        2,
        config.signerCommitments,
        { signatureScheme: 'ecdsa' },
      );
    });
  });

  describe('createSwitchGuardianProposal', () => {
    it('should verify new endpoint commitment before creating proposal', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const newGuardianPubkey = '0x' + '1'.repeat(64);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: newGuardianPubkey }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: {
            account_id: multisig.accountId,
            nonce: 1,
            prev_commitment: '0x' + 'b'.repeat(64),
            delta_payload: { tx_summary: { data: 'AQID' }, signatures: [] },
            status: {
              status: 'pending',
              timestamp: '2024-01-01T00:00:00Z',
              proposer_id: '0x' + 'c'.repeat(64),
              cosigner_sigs: [],
            },
          },
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createSwitchGuardianProposal('http://new-guardian.com', newGuardianPubkey);

      expect(proposal.metadata?.proposalType).toBe('switch_guardian');
      if (proposal.metadata?.proposalType === 'switch_guardian') {
        expect(proposal.metadata.newGuardianEndpoint).toBe('http://new-guardian.com');
      }
      expect(mockFetch).toHaveBeenCalledWith(
        'http://new-guardian.com/pubkey?scheme=falcon',
        expect.objectContaining({ method: 'GET' })
      );
    });

    it('should reject switch proposal when endpoint commitment does not match', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: '0x' + '2'.repeat(64) }),
      });

      await expect(
        multisig.createSwitchGuardianProposal('http://new-guardian.com', '0x' + '1'.repeat(64))
      ).rejects.toThrow('Refusing to use GUARDIAN endpoint');
    });

    it('should use the signer scheme when resolving new GUARDIAN commitments', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const ecdsaSigner: Signer = {
        ...mockSigner,
        publicKey: '0x' + '2'.repeat(66),
        scheme: 'ecdsa',
      };
      guardian.setSigner(ecdsaSigner);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config, ecdsaSigner);
      const newGuardianCommitment = '0x' + '1'.repeat(64);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: newGuardianCommitment }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: {
            account_id: multisig.accountId,
            nonce: 1,
            prev_commitment: '0x' + 'b'.repeat(64),
            delta_payload: { tx_summary: { data: 'AQID' }, signatures: [] },
            status: {
              status: 'pending',
              timestamp: '2024-01-01T00:00:00Z',
              proposer_id: '0x' + 'c'.repeat(64),
              cosigner_sigs: [],
            },
          },
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      await multisig.createSwitchGuardianProposal('http://new-guardian.com', newGuardianCommitment);

      expect(mockFetch).toHaveBeenCalledWith(
        'http://new-guardian.com/pubkey?scheme=ecdsa',
        expect.objectContaining({ method: 'GET' }),
      );
      expect(buildUpdateGuardianTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        newGuardianCommitment,
        { signatureScheme: 'ecdsa' },
      );
    });
  });

  describe('createUpdateProcedureThresholdProposal', () => {
    it('should create procedure-threshold update proposals', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        toCommitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'update_procedure_threshold',
            target_threshold: 1,
            target_procedure: 'send_asset',
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createUpdateProcedureThresholdProposal('send_asset', 1, 1);

      expect(buildUpdateProcedureThresholdTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        'send_asset',
        1,
        { signatureScheme: 'falcon' },
      );
      expect(proposal.metadata.proposalType).toBe('update_procedure_threshold');
      if (proposal.metadata.proposalType === 'update_procedure_threshold') {
        expect(proposal.metadata.targetProcedure).toBe('send_asset');
        expect(proposal.metadata.targetThreshold).toBe(1);
      }
    });

    it('passes the signer scheme to ECDSA procedure-threshold updates', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        toCommitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const ecdsaSigner: Signer = {
        ...mockSigner,
        publicKey: '0x' + '2'.repeat(66),
        scheme: 'ecdsa',
      };
      guardian.setSigner(ecdsaSigner);

      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config, ecdsaSigner);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'update_procedure_threshold',
            target_threshold: 1,
            target_procedure: 'send_asset',
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      await multisig.createUpdateProcedureThresholdProposal('send_asset', 1, 1);

      expect(buildUpdateProcedureThresholdTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        'send_asset',
        1,
        { signatureScheme: 'ecdsa' },
      );
    });
  });

  describe('signProposal', () => {
    it('should sign a proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: [mockSigner.commitment],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      // First create a proposal
      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      await multisig.createProposal(1, 'AQID', {
        proposalType: 'add_signer',
        targetThreshold: 1,
        targetSignerCommitments: ['0x' + 'a'.repeat(64)],
        description: '',
      });

      const signedDelta = {
        ...mockDelta,
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [
            {
              signer_id: mockSigner.commitment,
              signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
              timestamp: '2024-01-01T01:00:00Z',
            },
          ],
        },
        delta_payload: {
          ...mockDelta.delta_payload,
          metadata: {
            proposal_type: 'add_signer',
            description: '',
            target_threshold: 1,
            signer_commitments: ['0x' + 'a'.repeat(64)],
          },
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => signedDelta,
      });

      const proposalId = '0x' + 'c'.repeat(64);
      const signedProposal = await multisig.signProposal(proposalId);

      expect(mockSigner.signCommitment).toHaveBeenCalledWith(proposalId);
      expect(signedProposal.signatures.length).toBe(1);
    });

    it('should reject signing when metadata does not match tx_summary', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      await multisig.createProposal(1, 'AQID', {
        proposalType: 'add_signer',
        targetThreshold: 1,
        targetSignerCommitments: ['0x' + 'a'.repeat(64)],
        description: '',
      });

      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      } as any);

      await expect(multisig.signProposal('0x' + 'c'.repeat(64))).rejects.toThrow(
        'Invalid proposal: metadata does not match tx_summary'
      );
    });

    it('should reject proposals for a different account before signing', async () => {
      const config = {
        threshold: 1,
        signerCommitments: [mockSigner.commitment],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'd'.repeat(64);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          proposals: [
            {
              account_id: '0x' + 'f'.repeat(30),
              nonce: 1,
              prev_commitment: '0x' + 'b'.repeat(64),
              delta_payload: {
                tx_summary: { data: 'AQID' },
                signatures: [],
                metadata: {
                  proposal_type: 'add_signer',
                  description: '',
                  target_threshold: 1,
                  signer_commitments: [mockSigner.commitment],
                },
              },
              status: {
                status: 'pending',
                timestamp: '2024-01-01T00:00:00Z',
                proposer_id: '0x' + 'c'.repeat(64),
                cosigner_sigs: [],
              },
            },
          ],
        }),
      });

      await expect(multisig.signProposal(proposalId)).rejects.toThrow(
        'Proposal is for a different account: 0x' + 'f'.repeat(30),
      );
      expect(mockSigner.signCommitment).not.toHaveBeenCalled();
    });
  });

  describe('importProposal', () => {
    it('should reject imported proposals whose metadata does not match tx_summary', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      } as any);

      await expect(
        multisig.importProposal(
          JSON.stringify({
            accountId: '0x' + 'a'.repeat(30),
            nonce: 1,
            commitment: '0x' + 'c'.repeat(64),
            txSummaryBase64: 'AQID',
            signatures: [],
            metadata: {
              proposalType: 'add_signer',
              targetThreshold: 1,
              targetSignerCommitments: ['0x' + 'a'.repeat(64)],
              description: '',
            },
          })
        )
      ).rejects.toThrow('Invalid proposal: metadata does not match tx_summary');
    });
  });

  describe('signProposalOffline', () => {
    it('should reject signing imported proposals whose metadata does not match tx_summary', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
      } as any);

      const proposal = await multisig.importProposal(
        JSON.stringify({
          accountId: '0x' + 'a'.repeat(30),
          nonce: 1,
          commitment: '0x' + 'c'.repeat(64),
          txSummaryBase64: 'AQID',
          signatures: [],
          metadata: {
            proposalType: 'add_signer',
            targetThreshold: 1,
            targetSignerCommitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
        })
      );

      proposal.metadata = {
        proposalType: 'add_signer',
        targetThreshold: 2,
        targetSignerCommitments: ['0x' + 'a'.repeat(64)],
        description: '',
      };

      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      } as any);

      await expect(multisig.signProposalOffline(proposal.id)).rejects.toThrow(
        'Invalid proposal: metadata does not match tx_summary'
      );
    });
  });

  describe('exportProposal', () => {
    it('should export proposal for offline signing', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'add_signer',
              description: '',
              target_threshold: 1,
              signer_commitments: ['0x' + 'a'.repeat(64)],
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => mockProposals[0],
      });

      // The proposal ID is computed from tx_summary, which is mocked to return 'c'.repeat(64)
      const exported = await multisig.exportProposal('0x' + 'c'.repeat(64));

      expect(exported.accountId).toBe('0x' + 'a'.repeat(30));
      expect(exported.nonce).toBe(1);
      expect(exported.txSummaryBase64).toBe('AQID');
      expect(exported.signatures.length).toBe(1);
    });

    it('should preserve ECDSA signature metadata in exported proposals', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const publicKey = '0x' + 'd'.repeat(66);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'change_threshold',
              description: '',
              target_threshold: 2,
              signer_commitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: {
                  scheme: 'ecdsa',
                  signature: '0x' + 'e'.repeat(130),
                  public_key: publicKey,
                },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        }),
      });

      const exported = await multisig.exportProposal('0x' + 'c'.repeat(64));

      expect(exported.signatures).toEqual([
        {
          commitment: '0x' + 'a'.repeat(64),
          signatureHex: '0x' + 'e'.repeat(130),
          scheme: 'ecdsa',
          publicKey,
          timestamp: '2024-01-01T00:00:00Z',
        },
      ]);
    });

    it('should throw if proposal not found', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 404,
        statusText: 'Not Found',
        headers: new Headers(),
        // Feature 009: only a conforming { code, message, meta } envelope is
        // folded into the error message; raw text bodies are dropped.
        text: async () =>
          JSON.stringify({
            code: 'proposal_not_found',
            message: 'Proposal not found',
            meta: { retryable: false },
          }),
      });

      await expect(
        multisig.exportProposal('0x' + 'nonexistent'.repeat(5))
      ).rejects.toThrow('Proposal not found');
    });
  });

  describe('importProposal', () => {
    it('should reject imported signatures with non-32-byte signer IDs', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const exported = {
        accountId: multisig.accountId,
        nonce: 1,
        commitment: '0x' + 'c'.repeat(64),
        txSummaryBase64: 'AQID',
        signatures: [
          {
            commitment: '0x1',
            signatureHex: '0x' + 'b'.repeat(128),
          },
        ],
        metadata: {
          proposalType: 'add_signer' as const,
          targetThreshold: 1,
          targetSignerCommitments: ['0x' + 'a'.repeat(64)],
          description: '',
        },
      };

      await expect(multisig.importProposal(JSON.stringify(exported))).rejects.toThrow(
        'expected signerId as 32-byte hex',
      );
    });

    it('should preserve ECDSA imported signature metadata', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const publicKey = '0x' + 'd'.repeat(66);

      const proposal = await multisig.importProposal(
        JSON.stringify({
          accountId: multisig.accountId,
          nonce: 1,
          commitment: '0x' + 'c'.repeat(64),
          txSummaryBase64: 'AQID',
          signatures: [
            {
              commitment: '0x' + 'a'.repeat(64),
              signatureHex: '0x' + 'b'.repeat(130),
              scheme: 'ecdsa',
              publicKey,
              timestamp: '2024-01-01T00:00:00Z',
            },
          ],
          metadata: {
            proposalType: 'change_threshold',
            targetThreshold: 1,
            targetSignerCommitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
        })
      );

      expect(proposal.signatures).toEqual([
        {
          signerId: '0x' + 'a'.repeat(64),
          signature: {
            scheme: 'ecdsa',
            signature: '0x' + 'b'.repeat(130),
            publicKey,
          },
          timestamp: '2024-01-01T00:00:00Z',
        },
      ]);
    });

    it('should reject imported ECDSA signatures without a public key', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      await expect(
        multisig.importProposal(
          JSON.stringify({
            accountId: multisig.accountId,
            nonce: 1,
            commitment: '0x' + 'c'.repeat(64),
            txSummaryBase64: 'AQID',
            signatures: [
              {
                commitment: '0x' + 'a'.repeat(64),
                signatureHex: '0x' + 'b'.repeat(130),
                scheme: 'ecdsa',
              },
            ],
            metadata: {
              proposalType: 'change_threshold',
              targetThreshold: 1,
              targetSignerCommitments: ['0x' + 'a'.repeat(64)],
              description: '',
            },
          })
        )
      ).rejects.toThrow('ECDSA signature for 0x' + 'a'.repeat(64) + ' is missing publicKey');
    });

    it('should reject offline signing if an imported proposal account is changed', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), mockSigner.commitment],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const exported = {
        accountId: multisig.accountId,
        nonce: 1,
        commitment: '0x' + 'c'.repeat(64),
        txSummaryBase64: 'AQID',
        signatures: [],
        metadata: {
          proposalType: 'add_signer' as const,
          targetThreshold: 2,
          targetSignerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
          description: '',
        },
      };

      const proposal = await multisig.importProposal(JSON.stringify(exported));
      proposal.accountId = '0x' + 'f'.repeat(30);

      await expect(multisig.signProposalOffline(proposal.id)).rejects.toThrow(
        'Proposal is for a different account: 0x' + 'f'.repeat(30),
      );
      expect(mockSigner.signCommitment).not.toHaveBeenCalled();
    });
  });

  describe('createTransactionProposalRequest', () => {
    it('should return a ready non-switch_guardian request without executing it', async () => {
      const { buildSignatureAdviceEntry, signatureHexToBytes } = await import('./utils/signature.js');
      vi.mocked(signatureHexToBytes).mockClear();
      vi.mocked(buildSignatureAdviceEntry).mockClear();

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
        guardianPublicKey: '0x' + '1'.repeat(66),
      };

      const ecdsaSigner: Signer = {
        ...mockSigner,
        scheme: 'ecdsa',
        publicKey: '0x' + '2'.repeat(66),
      };

      const multisig = createTestMultisig(config, ecdsaSigner);
      const cachedProposalId = '0x' + 'c'.repeat(64);
      const requestedProposalId = '0x' + 'C'.repeat(64);
      const cosignerPubkey = '0x' + '3'.repeat(66);
      const ackPubkey = '0x' + '4'.repeat(66);
      const cosignerSignature = '0x' + '5'.repeat(130);
      const ackSignature = '0x' + '6'.repeat(130);
      const finalRequest = { kind: 'final-change-threshold-request' };

      vi.mocked(buildUpdateSignersTransactionRequest)
        .mockResolvedValueOnce({
          request: { kind: 'verify-change-threshold-request' },
          salt: { toHex: () => '0x' + 'd'.repeat(64) },
          configHash: { toHex: () => '0x' + 'e'.repeat(64) },
        } as any)
        .mockResolvedValueOnce({
          request: finalRequest,
          salt: { toHex: () => '0x' + 'd'.repeat(64) },
          configHash: { toHex: () => '0x' + 'e'.repeat(64) },
        } as any);

      (multisig as any).proposals.set(cachedProposalId, {
        id: cachedProposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: {
              scheme: 'ecdsa',
              signature: cosignerSignature,
              publicKey: cosignerPubkey,
            },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'change_threshold',
          targetThreshold: 1,
          targetSignerCommitments: ['0x' + 'a'.repeat(64)],
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'change_threshold',
              target_threshold: 1,
              signer_commitments: ['0x' + 'a'.repeat(64)],
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'a'.repeat(64),
            cosigner_sigs: [],
          },
        }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          ack_sig: ackSignature,
          ack_pubkey: ackPubkey,
          ack_scheme: 'ecdsa',
        }),
      });

      await expect(
        multisig.createTransactionProposalRequest(requestedProposalId)
      ).resolves.toBe(finalRequest);

      expect(vi.mocked(signatureHexToBytes)).toHaveBeenNthCalledWith(
        1,
        cosignerSignature,
        'ecdsa',
      );
      expect(vi.mocked(signatureHexToBytes)).toHaveBeenNthCalledWith(
        2,
        ackSignature,
        'ecdsa',
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        1,
        expect.anything(),
        expect.anything(),
        expect.anything(),
        cosignerPubkey,
        cosignerSignature,
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        2,
        expect.anything(),
        expect.anything(),
        expect.anything(),
        ackPubkey,
        ackSignature,
      );
      expect(mockWebClient.executeTransaction).not.toHaveBeenCalled();
      expect(mockWebClient.proveTransaction).not.toHaveBeenCalled();
      expect(mockWebClient.submitProvenTransaction).not.toHaveBeenCalled();
      expect(mockWebClient.applyTransaction).not.toHaveBeenCalled();
    });

    it('should return a ready switch_guardian request without executing it', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);
      const newGuardianPubkey = '0x' + '1'.repeat(64);
      const finalRequest = { kind: 'final-switch-guardian-request' };

      // switch_guardian is exempt from binding re-execution, so only the single
      // final-request build happens here.
      vi.mocked(buildUpdateGuardianTransactionRequest)
        .mockResolvedValueOnce({
          request: finalRequest,
          salt: { toHex: () => '0x' + 'd'.repeat(64) },
        } as any);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey,
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: newGuardianPubkey }),
      });

      await expect(multisig.createTransactionProposalRequest(proposalId)).resolves.toBe(finalRequest);

      expect(mockFetch).toHaveBeenCalledTimes(1);
      expect(mockWebClient.executeTransaction).not.toHaveBeenCalled();
      expect(mockWebClient.proveTransaction).not.toHaveBeenCalled();
      expect(mockWebClient.submitProvenTransaction).not.toHaveBeenCalled();
      expect(mockWebClient.applyTransaction).not.toHaveBeenCalled();
    });

    it('should throw if proposal not found locally', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      await expect(
        multisig.createTransactionProposalRequest('0x' + 'nonexistent'.repeat(5))
      ).rejects.toThrow('Proposal not found');
    });

    it('should throw if proposal is still pending', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'add_signer',
              description: '',
              target_threshold: 2,
              signer_commitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposals }),
      });

      await multisig.syncProposals();

      await expect(
        multisig.createTransactionProposalRequest('0x' + 'c'.repeat(64))
      ).rejects.toThrow('not ready for execution');
    });

    it('should throw when proposal metadata does not match tx_summary', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);

      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'd'.repeat(64),
        }),
      } as any);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'change_threshold',
          targetThreshold: 1,
          targetSignerCommitments: ['0x' + 'a'.repeat(64)],
          description: '',
        },
      });

      await expect(multisig.createTransactionProposalRequest(proposalId)).rejects.toThrow(
        `Invalid proposal: metadata does not match tx_summary for ${proposalId}`
      );
      expect(mockWebClient.executeTransaction).not.toHaveBeenCalled();
    });

    it('should reject switch_guardian requests when endpoint commitment mismatches', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey: '0x' + '1'.repeat(64),
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: '0x' + '2'.repeat(64) }),
      });

      await expect(multisig.createTransactionProposalRequest(proposalId)).rejects.toThrow(
        'Refusing to use GUARDIAN endpoint'
      );
      expect(mockWebClient.executeTransaction).not.toHaveBeenCalled();
    });

    it('should reject duplicate normalized signer IDs during request creation', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
          {
            signerId: '0x' + 'A'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'c'.repeat(128) },
            timestamp: '2024-01-01T00:00:01Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey: '0x' + '1'.repeat(64),
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      await expect(multisig.createTransactionProposalRequest(proposalId)).rejects.toThrow(
        'duplicate signatures for signer',
      );
    });

    it('should build a fresh tx commitment word for each advice entry during request creation', async () => {
      const { buildSignatureAdviceEntry } = await import('./utils/signature.js');
      const { Word } = await import('@miden-sdk/miden-sdk');

      const originalAdviceImpl = vi.mocked(buildSignatureAdviceEntry).getMockImplementation();
      const originalWordFromHexImpl = vi.mocked(Word.fromHex).getMockImplementation();

      try {
        vi.mocked(Word.fromHex).mockImplementation((hex: string) => {
          let consumed = false;
          return {
            toHex: () => hex,
            toFelts: () => {
              if (consumed) {
                throw new Error('Word already consumed');
              }
              consumed = true;
              return [1, 2, 3, 4];
            },
          } as any;
        });

        vi.mocked(buildSignatureAdviceEntry).mockImplementation(
          (signerCommitment: any, message: any) => {
            message.toFelts();
            return {
              key: { toHex: () => signerCommitment.toHex() },
              values: [1, 2, 3],
            } as any;
          },
        );

        const config = {
          threshold: 1,
          signerCommitments: ['0x' + 'a'.repeat(64)],
          guardianCommitment: '0x' + 'c'.repeat(64),
        };

        const multisig = createTestMultisig(config);
        const proposalId = '0x' + 'c'.repeat(64);
        const finalRequest = { kind: 'fresh-message-word-request' };

        vi.mocked(buildUpdateSignersTransactionRequest)
          .mockResolvedValueOnce({
            request: { kind: 'verify-change-threshold-request' },
            salt: { toHex: () => '0x' + 'd'.repeat(64) },
            configHash: { toHex: () => '0x' + 'e'.repeat(64) },
          } as any)
          .mockResolvedValueOnce({
            request: finalRequest,
            salt: { toHex: () => '0x' + 'd'.repeat(64) },
            configHash: { toHex: () => '0x' + 'e'.repeat(64) },
          } as any);

        (multisig as any).proposals.set(proposalId, {
          id: proposalId,
          accountId: multisig.accountId,
          nonce: 1,
          status: 'ready',
          txSummary: 'AQID',
          signatures: [
            {
              signerId: '0x' + 'a'.repeat(64),
              signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
              timestamp: '2024-01-01T00:00:00Z',
            },
          ],
          metadata: {
            proposalType: 'change_threshold',
            targetThreshold: 1,
            targetSignerCommitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
        });

        mockFetch.mockResolvedValueOnce({
          ok: true,
          json: async () => ({
            account_id: multisig.accountId,
            nonce: 1,
            prev_commitment: '0x' + 'b'.repeat(64),
            delta_payload: {
              tx_summary: { data: 'AQID' },
              signatures: [],
              metadata: {
                proposal_type: 'change_threshold',
                target_threshold: 1,
                signer_commitments: ['0x' + 'a'.repeat(64)],
              },
            },
            status: {
              status: 'pending',
              timestamp: '2024-01-01T00:00:00Z',
              proposer_id: '0x' + 'a'.repeat(64),
              cosigner_sigs: [],
            },
          }),
        });
        mockFetch.mockResolvedValueOnce({
          ok: true,
          json: async () => ({
            account_id: multisig.accountId,
            nonce: 1,
            ack_sig: '0x' + 'f'.repeat(128),
            ack_scheme: 'falcon',
          }),
        });

        await expect(multisig.createTransactionProposalRequest(proposalId)).resolves.toBe(finalRequest);
      } finally {
        if (originalAdviceImpl) {
          vi.mocked(buildSignatureAdviceEntry).mockImplementation(originalAdviceImpl);
        }
        if (originalWordFromHexImpl) {
          vi.mocked(Word.fromHex).mockImplementation(originalWordFromHexImpl);
        }
      }
    });

    it('should reject advice-map key collisions during request creation', async () => {
      const { buildSignatureAdviceEntry } = await import('./utils/signature.js');
      vi.mocked(buildSignatureAdviceEntry)
        .mockImplementationOnce(() => ({
          key: { toHex: () => '0x' + 'f'.repeat(64) },
          values: [1, 2, 3],
        }) as any)
        .mockImplementationOnce(() => ({
          key: { toHex: () => '0x' + 'f'.repeat(64) },
          values: [1, 2, 3],
        }) as any);

      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
          {
            signerId: '0x' + 'b'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'c'.repeat(128) },
            timestamp: '2024-01-01T00:00:01Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey: '0x' + '1'.repeat(64),
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      await expect(multisig.createTransactionProposalRequest(proposalId)).rejects.toThrow(
        'Duplicate advice-map key detected',
      );
    });
  });

  describe('executeProposal', () => {
    it('should throw if proposal not found locally', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      await expect(
        multisig.executeProposal('0x' + 'nonexistent'.repeat(5))
      ).rejects.toThrow('Proposal not found');
    });

    it('should throw if proposal is still pending', async () => {
      const config = {
        threshold: 2, // Need 2 signatures
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      // Sync with pending proposal (only 1 signature)
      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'add_signer',
              description: '',
              target_threshold: 2,
              signer_commitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposals }),
      });

      await multisig.syncProposals();

      // Proposal ID is mocked to return 'c'.repeat(64)
      await expect(
        multisig.executeProposal('0x' + 'c'.repeat(64))
      ).rejects.toThrow('not ready for execution');
    });

    it('should fail when GUARDIAN ack signature is missing (selector ON)', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const readyDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'add_signer',
            description: '',
            target_threshold: 1,
            signer_commitments: ['0x' + 'a'.repeat(64)],
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [
            {
              signer_id: '0x' + 'a'.repeat(64),
              signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
              timestamp: '2024-01-01T00:00:00Z',
            },
          ],
        },
      };

      const proposalId = '0x' + 'c'.repeat(64);

      // Prime local cache via syncProposals
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: [readyDelta] }),
      });
      await multisig.syncProposals();

      // executeProposal: getDeltaProposal
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => readyDelta,
      });
      // executeProposal: pushDelta without ack_sig
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ...readyDelta, ack_sig: null }),
      });

      await expect(multisig.executeProposal(proposalId)).rejects.toThrow(
        'GUARDIAN did not return acknowledgment signature'
      );
    });

    it('should encode ECDSA proposal and ack signatures with scheme-aware advice', async () => {
      const { buildSignatureAdviceEntry, signatureHexToBytes } = await import('./utils/signature.js');
      vi.mocked(signatureHexToBytes).mockClear();
      vi.mocked(buildSignatureAdviceEntry).mockClear();

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
        guardianPublicKey: '0x' + '1'.repeat(66),
      };

      const ecdsaSigner: Signer = {
        ...mockSigner,
        scheme: 'ecdsa',
        publicKey: '0x' + '2'.repeat(66),
      };

      const multisig = createTestMultisig(config, ecdsaSigner, undefined, {
        kind: 'remote',
        maxAttempts: 2,
        createProver: () => ({} as never),
      });
      const proposalId = '0x' + 'c'.repeat(64);
      const cosignerPubkey = '0x' + '3'.repeat(66);
      const ackPubkey = '0x' + '4'.repeat(66);
      const cosignerSignature = '0x' + '5'.repeat(130);
      const ackSignature = '0x' + '6'.repeat(130);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: {
              scheme: 'ecdsa',
              signature: cosignerSignature,
              publicKey: cosignerPubkey,
            },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'change_threshold',
          targetThreshold: 1,
          targetSignerCommitments: ['0x' + 'a'.repeat(64)],
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'change_threshold',
              target_threshold: 1,
              signer_commitments: ['0x' + 'a'.repeat(64)],
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'a'.repeat(64),
            cosigner_sigs: [],
          },
        }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          ack_sig: ackSignature,
          ack_pubkey: ackPubkey,
          ack_scheme: 'ecdsa',
        }),
      });
      mockWebClient.proveTransaction
        .mockRejectedValueOnce(Object.assign(new Error('unavailable'), { code: 'Unavailable' }))
        .mockResolvedValueOnce({});
      vi.useFakeTimers();
      try {
        const execution = multisig.executeProposal(proposalId);
        await vi.runAllTimersAsync();
        await expect(execution).resolves.toBeUndefined();
      } finally {
        vi.useRealTimers();
      }
      expect(mockWebClient.executeTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.proveTransaction).toHaveBeenCalledTimes(2);
      expect(mockWebClient.submitProvenTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.applyTransaction).toHaveBeenCalledTimes(1);

      expect(vi.mocked(signatureHexToBytes)).toHaveBeenNthCalledWith(
        1,
        cosignerSignature,
        'ecdsa',
      );
      expect(vi.mocked(signatureHexToBytes)).toHaveBeenNthCalledWith(
        2,
        ackSignature,
        'ecdsa',
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        1,
        expect.anything(),
        expect.anything(),
        expect.anything(),
        cosignerPubkey,
        cosignerSignature,
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        2,
        expect.anything(),
        expect.anything(),
        expect.anything(),
        ackPubkey,
        ackSignature,
      );
    });

    it('should execute imported ECDSA proposals with scheme-aware advice', async () => {
      const { buildSignatureAdviceEntry, signatureHexToBytes } = await import('./utils/signature.js');
      vi.mocked(signatureHexToBytes).mockClear();
      vi.mocked(buildSignatureAdviceEntry).mockClear();

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
        guardianPublicKey: '0x' + '1'.repeat(66),
      };

      const ecdsaSigner: Signer = {
        ...mockSigner,
        scheme: 'ecdsa',
        publicKey: '0x' + '2'.repeat(66),
      };

      const multisig = createTestMultisig(config, ecdsaSigner);
      const proposalId = '0x' + 'c'.repeat(64);
      const cosignerPubkey = '0x' + '3'.repeat(66);
      const ackPubkey = '0x' + '4'.repeat(66);
      const cosignerSignature = '0x' + '5'.repeat(130);
      const ackSignature = '0x' + '6'.repeat(130);

      await multisig.importProposal(
        JSON.stringify({
          accountId: multisig.accountId,
          nonce: 1,
          commitment: proposalId,
          txSummaryBase64: 'AQID',
          signatures: [
            {
              commitment: '0x' + 'a'.repeat(64),
              signatureHex: cosignerSignature,
              scheme: 'ecdsa',
              publicKey: cosignerPubkey,
              timestamp: '2024-01-01T00:00:00Z',
            },
          ],
          metadata: {
            proposalType: 'change_threshold',
            targetThreshold: 1,
            targetSignerCommitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
        })
      );

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'change_threshold',
              target_threshold: 1,
              signer_commitments: ['0x' + 'a'.repeat(64)],
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'a'.repeat(64),
            cosigner_sigs: [],
          },
        }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          ack_sig: ackSignature,
          ack_pubkey: ackPubkey,
          ack_scheme: 'ecdsa',
        }),
      });
      await expect(multisig.executeProposal(proposalId)).resolves.toBeUndefined();
      expect(mockWebClient.executeTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.proveTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.submitProvenTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.applyTransaction).toHaveBeenCalledTimes(1);

      expect(vi.mocked(signatureHexToBytes)).toHaveBeenNthCalledWith(
        1,
        cosignerSignature,
        'ecdsa',
      );
      expect(vi.mocked(signatureHexToBytes)).toHaveBeenNthCalledWith(
        2,
        ackSignature,
        'ecdsa',
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        1,
        expect.anything(),
        expect.anything(),
        expect.anything(),
        cosignerPubkey,
        cosignerSignature,
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        2,
        expect.anything(),
        expect.anything(),
        expect.anything(),
        ackPubkey,
        ackSignature,
      );
    });

    it('should verify switch_guardian endpoint commitment before execution', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);
      const newGuardianPubkey = '0x' + '1'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey,
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: newGuardianPubkey }),
      });
      // Pre-switch canonicalization push: getDeltaProposal then pushDelta.
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'switch_guardian',
              new_guardian_pubkey: newGuardianPubkey,
              new_guardian_endpoint: 'http://new-guardian.com',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'a'.repeat(64),
            cosigner_sigs: [],
          },
        }),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          ack_sig: '0x' + '6'.repeat(130),
          ack_pubkey: '0x' + 'f'.repeat(64),
          ack_scheme: 'falcon',
        }),
      });
      mockWebClient.getAccount.mockResolvedValueOnce({
        serialize: () => new Uint8Array([1, 2, 3]),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ success: true, message: 'ok', ack_pubkey: '0x' + 'f'.repeat(64) }),
      });
      await expect(multisig.executeProposal(proposalId)).resolves.toBeUndefined();
      expect(mockWebClient.executeTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.proveTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.submitProvenTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.applyTransaction).toHaveBeenCalledTimes(1);
    });

    it('should still switch GUARDIAN when the pre-switch canonicalization push fails', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);
      const newGuardianPubkey = '0x' + '1'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey,
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: newGuardianPubkey }),
      });
      // getDeltaProposal against the old GUARDIAN fails — must be swallowed.
      mockFetch.mockRejectedValueOnce(new Error('pre-switch GUARDIAN unreachable'));
      mockWebClient.getAccount.mockResolvedValueOnce({
        serialize: () => new Uint8Array([1, 2, 3]),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ success: true, message: 'ok', ack_pubkey: '0x' + 'f'.repeat(64) }),
      });
      await expect(multisig.executeProposal(proposalId)).resolves.toBeUndefined();
      expect(mockWebClient.executeTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.proveTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.submitProvenTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.applyTransaction).toHaveBeenCalledTimes(1);
    });

    it('should reject switch_guardian execution when endpoint commitment mismatches', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey: '0x' + '1'.repeat(64),
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: '0x' + '2'.repeat(64) }),
      });

      await expect(multisig.executeProposal(proposalId)).rejects.toThrow(
        'Refusing to use GUARDIAN endpoint'
      );
      expect(mockWebClient.executeTransaction).not.toHaveBeenCalled();
    });

    it('should reject duplicate normalized signer IDs during execution', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
          {
            signerId: '0x' + 'A'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'c'.repeat(128) },
            timestamp: '2024-01-01T00:00:01Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey: '0x' + '1'.repeat(64),
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      await expect(multisig.executeProposal(proposalId)).rejects.toThrow(
        'duplicate signatures for signer',
      );
    });

    it('should reject advice-map key collisions during execution', async () => {
      const { buildSignatureAdviceEntry } = await import('./utils/signature.js');
      vi.mocked(buildSignatureAdviceEntry)
        .mockImplementationOnce(() => ({
          key: { toHex: () => '0x' + 'f'.repeat(64) },
          values: [1, 2, 3],
        }) as any)
        .mockImplementationOnce(() => ({
          key: { toHex: () => '0x' + 'f'.repeat(64) },
          values: [1, 2, 3],
        }) as any);

      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);

      (multisig as any).proposals.set(proposalId, {
        id: proposalId,
        accountId: multisig.accountId,
        nonce: 1,
        status: 'ready',
        txSummary: 'AQID',
        signatures: [
          {
            signerId: '0x' + 'a'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'b'.repeat(128) },
            timestamp: '2024-01-01T00:00:00Z',
          },
          {
            signerId: '0x' + 'b'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'c'.repeat(128) },
            timestamp: '2024-01-01T00:00:01Z',
          },
        ],
        metadata: {
          proposalType: 'switch_guardian',
          newGuardianPubkey: '0x' + '1'.repeat(64),
          newGuardianEndpoint: 'http://new-guardian.com',
          description: '',
        },
      });

      await expect(multisig.executeProposal(proposalId)).rejects.toThrow(
        'Duplicate advice-map key detected',
      );
    });
  });

  describe('submitTransaction', () => {
    it('uses the configured total proof-attempt budget without repeating other stages', async () => {
      const multisig = createTestMultisig(
        {
          threshold: 1,
          signerCommitments: ['0x' + 'a'.repeat(64)],
          guardianCommitment: '0x' + 'c'.repeat(64),
        },
        mockSigner,
        undefined,
        {
          kind: 'remote',
          maxAttempts: 4,
          createProver: () => ({} as never),
        },
      );
      const transient = Object.assign(new Error('unavailable'), { code: 'Unavailable' });
      mockWebClient.proveTransaction
        .mockRejectedValueOnce(transient)
        .mockRejectedValueOnce(transient)
        .mockRejectedValueOnce(transient)
        .mockResolvedValueOnce({});

      vi.useFakeTimers();
      try {
        const submission = multisig.submitTransaction({} as never);
        await vi.runAllTimersAsync();
        await submission;
      } finally {
        vi.useRealTimers();
      }

      expect(mockWebClient.executeTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.proveTransaction).toHaveBeenCalledTimes(4);
      expect(mockWebClient.submitProvenTransaction).toHaveBeenCalledTimes(1);
      expect(mockWebClient.applyTransaction).toHaveBeenCalledTimes(1);
    });
  });

  describe('prepareCustomExecution', () => {
    const requestBytes = new Uint8Array([9, 8, 7]);

    function customDelta(
      proposalType: string,
      cosignerSigs: any[],
    ): any {
      return {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: proposalType,
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: cosignerSigs,
        },
      };
    }

    function falconSig(signerId: string): any {
      return {
        signer_id: signerId,
        signature: { scheme: 'falcon', signature: '0x' + 'e'.repeat(128) },
        timestamp: '2024-01-01T00:00:00Z',
      };
    }

    it('rejects a built-in proposal type', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      const multisig = createTestMultisig(config);

      const builtinDelta = {
        ...customDelta('change_threshold', [falconSig('0x' + 'a'.repeat(64))]),
      };
      builtinDelta.delta_payload.metadata = {
        proposal_type: 'change_threshold',
        description: '',
        target_threshold: 1,
        signer_commitments: ['0x' + 'a'.repeat(64)],
      } as any;

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => builtinDelta,
      });

      await expect(
        multisig.prepareCustomExecution('0x' + 'c'.repeat(64), requestBytes),
      ).rejects.toThrow('prepareCustomExecution is only for custom proposals');
    });

    it('rejects a proposal that is below its signature threshold', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      const multisig = createTestMultisig(config);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => customDelta('b2agg', [falconSig('0x' + 'a'.repeat(64))]),
      });

      await expect(
        multisig.prepareCustomExecution('0x' + 'c'.repeat(64), requestBytes),
      ).rejects.toThrow('have 1 of 2 required signatures');
    });

    it('rejects when the rebuilt request does not reproduce the signed commitment', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      const multisig = createTestMultisig(config);

      // Signed commitment comes from TransactionSummary.deserialize -> 'c' * 64.
      // Make the binding request derive a different commitment so the check fails.
      vi.mocked(executeForSummary).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + '9'.repeat(64),
        }),
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => customDelta('b2agg', [falconSig('0x' + 'a'.repeat(64))]),
      });

      await expect(
        multisig.prepareCustomExecution('0x' + 'c'.repeat(64), requestBytes),
      ).rejects.toThrow('Custom proposal binding mismatch');
    });

    it('fails when GUARDIAN does not return an acknowledgment signature', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      const multisig = createTestMultisig(config);

      const ready = customDelta('b2agg', [falconSig('0x' + 'a'.repeat(64))]);

      // getDeltaProposal
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ready,
      });
      // pushDelta returns no ack_sig
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ ...ready, ack_sig: null }),
      });

      await expect(
        multisig.prepareCustomExecution('0x' + 'c'.repeat(64), requestBytes),
      ).rejects.toThrow('GUARDIAN did not return acknowledgment signature');
    });
  });

  describe('proposal metadata preservation', () => {
    it('should preserve local metadata when syncing proposals', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      // Create a proposal with metadata
      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'add_signer',
            target_threshold: 2,
            signer_commitments: ['0x1', '0x2'],
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createProposal(1, 'AQID', {
        proposalType: 'add_signer',
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        description: '',
      });

      expect(proposal.metadata?.proposalType).toBe('add_signer');

      // Now sync - should preserve local metadata
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          proposals: [mockDelta],
        }),
      });

      const syncedProposals = await multisig.syncProposals();
      const syncedProposal = syncedProposals.find(p => p.nonce === 1);

      expect(syncedProposal?.metadata?.proposalType).toBe('add_signer');
    });

    it('should use GUARDIAN metadata for new proposals from other signers', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      // Sync proposals - no local proposals exist
      const mockProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'p2id',
              recipient_id: '0xrecipient',
              faucet_id: '0xfaucet',
              amount: '100',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'other'.repeat(12),
            cosigner_sigs: [],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposals }),
      });

      const proposals = await multisig.syncProposals();

      expect(proposals.length).toBe(1);
      expect(proposals[0].metadata?.proposalType).toBe('p2id');
    });
  });

  describe('createProposal with different metadata types', () => {
    it('should create consume_notes proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'add_signer',
            target_threshold: 2,
            signer_commitments: ['0x1', '0x2'],
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createProposal(1, 'AQID', {
        proposalType: 'consume_notes',
        noteIds: ['0xnote1', '0xnote2'],
        description: '',
      });

      expect(proposal.metadata?.proposalType).toBe('consume_notes');
    });

    it('should create p2id proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposal_type: 'add_signer',
            target_threshold: 1,
            signer_commitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createProposal(1, 'AQID', {
        proposalType: 'p2id',
        recipientId: '0xrecipient',
        faucetId: '0xfaucet',
        amount: '100',
        description: '',
      });

      expect(proposal.metadata?.proposalType).toBe('p2id');
    });

    it('should create switch_guardian proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const mockDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            proposalType: 'add_signer',
            targetThreshold: 2,
            targetSignerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
            description: '',
          },
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: mockDelta,
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      const proposal = await multisig.createProposal(1, 'AQID', {
        proposalType: 'switch_guardian',
        newGuardianPubkey: '0xnewpubkey',
        newGuardianEndpoint: 'http://new-guardian.com',
        description: '',
      });

      expect(proposal.metadata?.proposalType).toBe('switch_guardian');
    });
  });

  describe('proposal status transitions', () => {
    it('should transition from pending to ready when threshold met', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      // First sync with 1 signature (pending)
      const mockProposalsPending = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'add_signer',
              target_threshold: 2,
              signer_commitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
              description: '',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'sig'.repeat(40) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposalsPending }),
      });

      let proposals = await multisig.syncProposals();
      expect(proposals[0].status).toBe('pending');

      // Second sync with 2 signatures (ready)
      const mockProposalsReady = [
        {
          ...mockProposalsPending[0],
          delta_payload: {
            ...mockProposalsPending[0].delta_payload,
            metadata: {
              proposal_type: 'add_signer',
              target_threshold: 2,
              signer_commitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
              description: '',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'sig'.repeat(40) },
                timestamp: '2024-01-01T00:00:00Z',
              },
              {
                signer_id: '0x' + 'b'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'sig2'.repeat(40) },
                timestamp: '2024-01-01T01:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: mockProposalsReady }),
      });

      proposals = await multisig.syncProposals();
      expect(proposals[0].status).toBe('ready');
    });
  });

  describe('getters', () => {
    it('should expose threshold', () => {
      const config = {
        threshold: 3,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64), '0x' + 'c'.repeat(64)],
        guardianCommitment: '0x' + 'd'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      expect(multisig.threshold).toBe(3);
    });

    it('should expose signerCommitments', () => {
      const signerCommitments = ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)];
      const config = {
        threshold: 2,
        signerCommitments,
        guardianCommitment: '0x' + 'd'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      expect(multisig.signerCommitments).toEqual(signerCommitments);
    });

    it('should expose guardianCommitment', () => {
      const guardianCommitment = '0x' + 'guardian'.repeat(20);
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment,
      };

      const multisig = createTestMultisig(config);
      expect(multisig.guardianCommitment).toBe(guardianCommitment);
    });

    it('should expose account when provided', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'd'.repeat(64),
      };

      const multisig = createTestMultisig(config);
      expect(multisig.account).toBe(mockAccount);
    });
  });

  describe('cross-client compatibility: sync with snake_case metadata', () => {
    it('should parse Rust client proposals with snake_case metadata', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      // Simulates a GUARDIAN response with canonical snake_case metadata
      const rustProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'change_threshold',
              target_threshold: 3,
              signer_commitments: ['0xa', '0xb', '0xc'],
              salt: '0xlegacysalt',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'rust_client'.repeat(5),
            cosigner_sigs: [],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: rustProposals }),
      });

      const proposals = await multisig.syncProposals();

      expect(proposals.length).toBe(1);
      // The TS client should normalize snake_case to camelCase
      expect(proposals[0].metadata?.proposalType).toBe('change_threshold');
      if (proposals[0].metadata?.proposalType === 'change_threshold') {
        expect(proposals[0].metadata.targetThreshold).toBe(3);
        expect(proposals[0].metadata.targetSignerCommitments).toEqual(['0xa', '0xb', '0xc']);
      }
    });

    it('should parse Rust client P2ID proposal with snake_case fields', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      // P2ID proposal with canonical snake_case fields
      const p2idProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'p2id',
              recipient_id: '0xrecipient',
              faucet_id: '0xfaucet',
              amount: '12345',
              salt: '0xsalt',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [
              {
                signer_id: '0x' + 'a'.repeat(64),
                signature: { scheme: 'falcon', signature: '0x' + 'sig'.repeat(40) },
                timestamp: '2024-01-01T00:00:00Z',
              },
            ],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: p2idProposals }),
      });

      const proposals = await multisig.syncProposals();

      expect(proposals.length).toBe(1);
      expect(proposals[0].metadata?.proposalType).toBe('p2id');
      if (proposals[0].metadata?.proposalType === 'p2id') {
        expect(proposals[0].metadata.recipientId).toBe('0xrecipient');
        expect(proposals[0].metadata.faucetId).toBe('0xfaucet');
        expect(proposals[0].metadata.amount).toBe('12345');
      }
    });

    it('should parse switch_guardian proposal with snake_case fields', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = createTestMultisig(config);

      const switchGuardianProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'switch_guardian',
              new_guardian_pubkey: '0xnewpubkey',
              new_guardian_endpoint: 'http://new-guardian.com',
              salt: '0xsalt',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [],
          },
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: switchGuardianProposals }),
      });

      const proposals = await multisig.syncProposals();

      expect(proposals.length).toBe(1);
      expect(proposals[0].metadata?.proposalType).toBe('switch_guardian');
      if (proposals[0].metadata?.proposalType === 'switch_guardian') {
        expect(proposals[0].metadata.newGuardianPubkey).toBe('0xnewpubkey');
        expect(proposals[0].metadata.newGuardianEndpoint).toBe('http://new-guardian.com');
      }
    });
  });
});
