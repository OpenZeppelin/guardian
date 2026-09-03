import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Multisig } from './multisig.js';
import { FeeFaucetAnchorMismatchError } from './multisig/authArgErrors.js';
import { GuardianHttpClient, type Signer } from '@openzeppelin/guardian-client';
import { Endpoint } from '@miden-sdk/miden-sdk';
import { buildConsumeNotesTransactionRequestFromNotes } from './transaction/consumeNotes.js';
import {
  buildConsumeNotesTransactionRequest,
  buildP2idTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateGuardianTransactionRequest,
  buildUpdateSignersTransactionRequest,
  chainAnchorFromBase64,
  executeForSummary,
  executeForSummaryAt,
  feeAuthArg,
} from './transaction.js';

const {
  mockRpcGetAccountDetails,
  mockRpcGetBlockHeaderByNumber,
  MOCK_FEE_FAUCET_ID,
  MOCK_FEE_FAUCET_ID_HEX,
  DEFAULT_TX_SUMMARY,
  mockTxSummaryDeserialize,
  mockAccountDeserialize,
  mockDetectConfig,
  mockNoteFileDeserialize,
  mockGetSignerCommitments,
  mockGetGuardianCommitment,
  mockImportNotesFromProposals,
  mockBackfillPublicNotesByTag,
  MOCK_OTHER_FEE_FAUCET_ID,
  MOCK_OTHER_FEE_FAUCET_ID_HEX,
  MOCK_SYNC_HEIGHT,
} = vi.hoisted(() => {
  // The faucet crosses every boundary as hex, so assertions compare hex rather
  // than handle identity; the handle exists only because a block header hands one
  // out.
  const MOCK_FEE_FAUCET_ID_HEX = '0xfee';
  const MOCK_FEE_FAUCET_ID = { toString: () => MOCK_FEE_FAUCET_ID_HEX, free: () => {} };
  // The block a proposal is anchored at is the block its faucet must come from.
  // Keeping a second, different faucet here means a create path that reads the
  // wrong block cannot pass: the two would no longer collapse to one constant.
  const MOCK_OTHER_FEE_FAUCET_ID_HEX = '0xfee2';
  const MOCK_OTHER_FEE_FAUCET_ID = {
    toString: () => MOCK_OTHER_FEE_FAUCET_ID_HEX,
    free: () => {},
  };
  const MOCK_SYNC_HEIGHT = 4242;
  const DEFAULT_TX_SUMMARY = {
    toCommitment: () => ({
      toHex: () => '0x' + 'c'.repeat(64),
    }),
    blockCommitment: () => ({
      toHex: () => '0x' + 'b'.repeat(64),
    }),
    userParams: () => [0, 0, 0, 1, 2, 3, 4],
    serialize: () => new Uint8Array([1, 2, 3]),
  };
  return {
    mockRpcGetAccountDetails: vi.fn(),
    // getFeeFaucetId reads the header of the block the client is synced to,
    // which every typed propose path commits to; the assertions below pin both
    // the block it asks for and the faucet it commits.
    mockRpcGetBlockHeaderByNumber: vi.fn().mockResolvedValue({
      blockNum: () => MOCK_SYNC_HEIGHT,
      feeFaucetId: () => MOCK_FEE_FAUCET_ID,
      free: () => {},
    }),
    MOCK_FEE_FAUCET_ID,
    MOCK_FEE_FAUCET_ID_HEX,
    MOCK_OTHER_FEE_FAUCET_ID,
    MOCK_OTHER_FEE_FAUCET_ID_HEX,
    MOCK_SYNC_HEIGHT,
    DEFAULT_TX_SUMMARY,
    mockTxSummaryDeserialize: vi.fn().mockReturnValue(DEFAULT_TX_SUMMARY),
    mockAccountDeserialize: vi.fn(),
    mockDetectConfig: vi.fn(),
    mockNoteFileDeserialize: vi.fn(),
    mockGetSignerCommitments: vi.fn(),
    mockGetGuardianCommitment: vi.fn(),
    mockImportNotesFromProposals: vi.fn(),
    mockBackfillPublicNotesByTag: vi.fn(),
  };
});

vi.mock('./recovery/proposalNoteImport.js', () => ({
  importNotesFromProposals: mockImportNotesFromProposals,
}));

vi.mock('./recovery/publicNoteBackfill.js', () => ({
  backfillPublicNotesByTag: mockBackfillPublicNotesByTag,
}));

const { MOCK_CHAIN_ANCHOR_B64, createMockChainAnchor } = vi.hoisted(() => {
  const MOCK_CHAIN_ANCHOR_B64 = 'bW9jay1jaGFpbi1hbmNob3I=';
  const createMockChainAnchor = () =>
    ({
      commitment: () => ({ toHex: () => '0x' + 'b'.repeat(64) }),
      // A proposal's fee faucet is read from the block its anchor pins, not from
      // the chain, so rebuilding needs no RPC.
      blockHeader: () => ({
        feeFaucetId: () => MOCK_FEE_FAUCET_ID,
        free: () => {},
      }),
      free: () => {},
      serialize: () => new Uint8Array([9, 9, 9]),
    }) as never;
  return { MOCK_CHAIN_ANCHOR_B64, createMockChainAnchor };
});

// Mock the Miden SDK
vi.mock('@miden-sdk/miden-sdk', () => ({
  Account: {
    deserialize: mockAccountDeserialize,
  },
  AccountId: {
    fromHex: vi.fn((hex: string) => ({ toString: () => hex, free: () => {} })),
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
    deserialize: mockTxSummaryDeserialize,
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
    getBlockHeaderByNumber: mockRpcGetBlockHeaderByNumber,
  })),
}));

// Mock transaction module
// `multisig.ts` imports this one builder from its own module rather than the
// barrel, so mocking the barrel alone leaves the real implementation in place.
vi.mock('./transaction/consumeNotes.js', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./transaction/consumeNotes.js')>()),
  buildConsumeNotesTransactionRequestFromNotes: vi.fn().mockReturnValue({
    request: {},
    salt: { toHex: () => '0x' + 'd'.repeat(64) },
  }),
}));

vi.mock('./transaction.js', () => {
  // `recoverProposalAuthArg` derives the auth arg with `feeAuthArg` and checks it
  // against the one `summaryAuthArg` reads off the signed summary, so both are
  // mocked to the same word: every recorded salt derives back to the signed arg
  // unless a test overrides `feeAuthArg` to steer it away. The real hash and its
  // operand order are covered by src/transaction/feeAuth.test.ts.
  const summaryAuthArg = vi.fn(() => ({
    toHex: () => '0x' + 'a'.repeat(64),
  }));
  const feeAuthArg = vi.fn((_conversionInfo: unknown, _salt: unknown) => ({
    toHex: () => '0x' + 'a'.repeat(64),
  }));
  const nativeConversionInfo = vi.fn((_feeFaucetId: unknown) => ({
    toHex: () => '0x' + 'c'.repeat(64),
  }));

  return {
  executeForSummary: vi.fn(),
  executeForSummaryAt: vi.fn(),
  chainAnchorToBase64: vi.fn(() => MOCK_CHAIN_ANCHOR_B64),
  chainAnchorFromBase64: vi.fn(() => createMockChainAnchor()),
  summaryAuthArg,
  feeAuthArg,
  nativeConversionInfo,
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
  };
});

vi.mock('./utils/signature.js', async () => {
  const actual = await vi.importActual<typeof import('./utils/signature.js')>('./utils/signature.js');
  return {
    ...actual,
    buildSignatureAdviceEntry: vi.fn().mockImplementation((signerCommitment: { toHex?: () => string }) => ({
      key: { toHex: () => signerCommitment.toHex ? signerCommitment.toHex() : '0x' + 'f'.repeat(64) },
      values: [1, 2, 3],
    })),
    signatureHexToBytes: vi.fn((hex: string) => new Uint8Array([0, 1, 2, 3])),
    // These tests use synthetic signature bytes to exercise advice routing;
    // recoverability is covered by tests/ecdsa-advice-encoding.test.ts.
    assertEcdsaSignatureRecoverable: vi.fn(),
  };
});

vi.mock('./utils/encoding.js', async () => {
  const actual = await vi.importActual<typeof import('./utils/encoding.js')>('./utils/encoding.js');
  return {
    ...actual,
    normalizeHexWord: vi.fn((hex: string) => '0x' + hex.replace(/^0x/i, '').toLowerCase().padStart(64, '0')),
  };
});

// Keep the real assertCompleteDetectedConfig so refreshConfigFromAccount's
// fail-closed validation is exercised.
vi.mock('./inspector.js', async (importOriginal) => {
  const actual = await importOriginal<typeof import('./inspector.js')>();
  return {
    ...actual,
    AccountInspector: {
      fromAccount: mockDetectConfig,
      getSignerPublicKeyCommitments: mockGetSignerCommitments,
      getGuardianPublicKeyCommitment: mockGetGuardianCommitment,
    },
  };
});

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
    // One test keys this on the input bytes; restore the default for everyone else.
    mockTxSummaryDeserialize.mockReset();
    mockTxSummaryDeserialize.mockReturnValue(DEFAULT_TX_SUMMARY);
    vi.mocked(executeForSummary).mockResolvedValue({
      summary: {
        toCommitment: () => ({
          toHex: () => '0x' + 'c'.repeat(64),
        }),
        serialize: () => new Uint8Array([1, 2, 3]),
      },
      anchor: createMockChainAnchor(),
    } as any);
    vi.mocked(executeForSummaryAt).mockResolvedValue({
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
      // The fee faucet is read from the block the client is synced to, because
      // that is the block a new request gets anchored at.
      getSyncHeight: vi.fn().mockResolvedValue(MOCK_SYNC_HEIGHT),
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
            status: 'canonical' as const,
            timestamp: '2026-08-01T12:00:03Z',
            newCommitment: '0x' + 'b'.repeat(64),
            inputNotes: [],
            outputNotes: [],
            decodeWarnings: [],
          },
        ],
        nextCursor: 'cursor-token',
      };
      const spy = vi.spyOn(guardian, 'getDeltaHistory').mockResolvedValue(page);

      const result = await multisig.deltaHistory({ limit: 5, cursor: 'prev' });

      expect(result).toBe(page);
      expect(spy).toHaveBeenCalledWith('0x' + 'a'.repeat(30), { limit: 5, cursor: 'prev' });
    });
  });

  describe('recoverNotes wiring', () => {
    const config = {
      threshold: 1,
      signerCommitments: ['0x' + 'a'.repeat(64)],
      guardianCommitment: '0x' + 'c'.repeat(64),
    };

    it("wires the strategies with this client's endpoint, rpc settings, and a pre-backfill chain sync", async () => {
      const multisig = createTestMultisig(config);
      const outcomes = [{ identifier: '0x1', source: 'proposal', status: 'imported' }];
      mockImportNotesFromProposals.mockResolvedValue(outcomes);
      const report = {
        scannedFrom: 5,
        scannedTo: 9,
        discovered: 0,
        skippedPrivate: 0,
        skippedIrrelevant: 0,
        skippedUnscreenable: 0,
        outcomes: [],
        uncovered: [],
        retryable: false,
      };
      mockBackfillPublicNotesByTag.mockResolvedValue(report);
      const proposalsSpy = vi.spyOn(guardian, 'getDeltaProposals').mockResolvedValue([]);
      mockWebClient.syncChain = vi.fn().mockResolvedValue(undefined);

      const result = await multisig.recoverNotes({
        transportDrain: false,
        syncAfter: false,
        fromBlock: 5,
        toBlock: 9,
      });

      expect(proposalsSpy).toHaveBeenCalledWith('0x' + 'a'.repeat(30));
      expect(mockImportNotesFromProposals).toHaveBeenCalledWith(mockWebClient, [], {
        midenRpcEndpoint: MIDEN_RPC_ENDPOINT,
        // Reuses the client's resolved retry budget (default: 2 attempts).
        rpc: { retry: { maxAttempts: 2 } },
      });
      // A store that has never seen the chain cannot import proofs, so the
      // backfill strategy syncs the chain state first.
      expect(mockWebClient.syncChain).toHaveBeenCalled();
      expect(mockBackfillPublicNotesByTag).toHaveBeenCalledWith(mockWebClient, {
        accountId: '0x' + 'a'.repeat(30),
        midenRpcEndpoint: MIDEN_RPC_ENDPOINT,
        rpc: { retry: { maxAttempts: 2 } },
        fromBlock: 5,
        toBlock: 9,
      });
      expect(result.proposalImport).toEqual(outcomes);
      expect(result.backfill).toBe(report);
      expect(result.problems).toEqual([]);
    });

    it('omits unset block bounds so the backfill genesis/tip defaults apply', async () => {
      const multisig = createTestMultisig(config);
      mockBackfillPublicNotesByTag.mockResolvedValue({ outcomes: [] } as never);
      mockWebClient.syncChain = vi.fn().mockResolvedValue(undefined);

      await multisig.recoverNotes({
        transportDrain: false,
        proposalImport: false,
        syncAfter: false,
      });

      const options = mockBackfillPublicNotesByTag.mock.calls.at(-1)?.[1] as object;
      expect('fromBlock' in options).toBe(false);
      expect('toBlock' in options).toBe(false);
    });

    it('isolates a corrupt proposal as an invalid outcome instead of failing the step', async () => {
      const multisig = createTestMultisig(config);
      mockImportNotesFromProposals.mockResolvedValue([]);
      vi.spyOn(guardian, 'getDeltaProposals').mockResolvedValue([
        // A payload that cannot even produce a proposal id: the listing must
        // skip it with a reason instead of throwing away the whole step.
        { nonce: 3, deltaPayload: { txSummary: { data: '!!! garbage !!!' } } },
      ] as never);

      const result = await multisig.recoverNotes({
        transportDrain: false,
        publicBackfill: false,
        syncAfter: false,
      });

      expect(result.problems).toEqual([]);
      expect(result.proposalImport).toHaveLength(1);
      expect(result.proposalImport?.[0]?.status).toBe('invalid');
      expect(result.proposalImport?.[0]?.identifier).toContain('nonce 3');
      // The healthy remainder (here: none) still reaches the import.
      expect(mockImportNotesFromProposals).toHaveBeenCalledWith(
        mockWebClient,
        [],
        expect.objectContaining({ midenRpcEndpoint: MIDEN_RPC_ENDPOINT }),
      );
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

  describe('getSignerPublicKeyCommitments (issue #306)', () => {
    const config = {
      threshold: 1,
      signerCommitments: ['0x' + 'a'.repeat(64)],
      guardianCommitment: '0x' + 'c'.repeat(64),
    };

    it('reads commitments from the store-backed account', async () => {
      const storeAccount = mockedAccount('0x' + 'b'.repeat(64), 1);
      mockWebClient.getAccount.mockResolvedValueOnce(storeAccount);
      const expected = ['0x' + '1'.repeat(64), '0x' + '2'.repeat(64)];
      mockGetSignerCommitments.mockReturnValueOnce(expected);

      const multisig = createTestMultisig(config);
      const commitments = await multisig.getSignerPublicKeyCommitments();

      expect(commitments).toEqual(expected);
      expect(mockGetSignerCommitments).toHaveBeenCalledWith(storeAccount);
    });

    it('falls back to the account snapshot when the store has no record', async () => {
      mockWebClient.getAccount.mockResolvedValueOnce(null);
      mockGetSignerCommitments.mockReturnValueOnce(['0x' + '3'.repeat(64)]);

      const multisig = createTestMultisig(config);
      await multisig.getSignerPublicKeyCommitments();

      expect(mockGetSignerCommitments).toHaveBeenCalledWith(mockAccount);
    });
  });

  describe('getGuardianPublicKeyCommitment (issue #306)', () => {
    it('reads the guardian commitment from the store-backed account', async () => {
      const storeAccount = mockedAccount('0x' + 'b'.repeat(64), 1);
      mockWebClient.getAccount.mockResolvedValueOnce(storeAccount);
      mockGetGuardianCommitment.mockReturnValueOnce('0x' + '4'.repeat(64));

      const multisig = createTestMultisig({
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      });

      const commitment = await multisig.getGuardianPublicKeyCommitment();

      expect(commitment).toBe('0x' + '4'.repeat(64));
      expect(mockGetGuardianCommitment).toHaveBeenCalledWith(storeAccount);
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

    it('keeps the previous config when a refresh reads an incomplete signer set (issue #306 review)', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      };
      const multisig = createTestMultisig(config, mockSigner, '0x' + 'a'.repeat(30));

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
      // Storage reports 3 signers but only 1 entry was readable: adopting
      // this would let membership proposals rewrite the on-chain set without
      // the omitted keys. The refresh must keep the previous config instead.
      mockDetectConfig.mockReturnValueOnce({
        threshold: 2,
        numSigners: 3,
        signerCommitments: ['0x' + '1'.repeat(64)],
        guardianCommitment: '0x' + 'd'.repeat(64),
        vaultBalances: [],
        procedureThresholds: new Map(),
      });

      await multisig.syncState();

      expect(multisig.threshold).toBe(1);
      expect(multisig.signerCommitments).toEqual(['0x' + 'a'.repeat(64)]);
      expect(multisig.guardianCommitment).toBe('0x' + 'c'.repeat(64));
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
                  salt: '0x' + 'd'.repeat(64),
                  chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      } as any);

      await expect(multisig.syncProposals()).rejects.toThrow(
        'Invalid proposal: metadata does not match tx_summary'
      );
    });

    /// A structurally valid anchor pinned to the wrong block must be rejected
    /// before anything executes against it: its commitment disagrees with the
    /// block commitment signed into the tx_summary.
    it('should reject a proposal whose chain anchor does not match the summary block commitment', async () => {
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
                  salt: '0x' + 'd'.repeat(64),
                  chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      // Deserializes fine, but pins a different block than the one bound into
      // the summary (mock summary blockCommitment is 'b' * 64).
      const freed = vi.fn();
      vi.mocked(chainAnchorFromBase64).mockReturnValueOnce({
        commitment: () => ({ toHex: () => '0x' + 'e'.repeat(64) }),
        free: freed,
        serialize: () => new Uint8Array([9, 9, 9]),
      } as never);

      const reExecutionsBefore = vi.mocked(executeForSummaryAt).mock.calls.length;
      await expect(multisig.syncProposals()).rejects.toThrow(
        'chain anchor does not match the block commitment bound into the tx_summary'
      );
      expect(vi.mocked(executeForSummaryAt).mock.calls.length).toBe(reExecutionsBefore);
      expect(freed).toHaveBeenCalledTimes(1);
    });

    /// The same rejection for `switch_guardian`, which is verified differently:
    /// its metadata is never checked by rebuilding, so the anchor check is the
    /// only thing standing between a served anchor and `submitAt` executing
    /// against it. That makes the check's position — before the type dispatch,
    /// not after — load-bearing rather than incidental.
    it('should reject a switch_guardian whose chain anchor does not match the summary', async () => {
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
                  proposal_type: 'switch_guardian',
                  salt: '0x' + 'd'.repeat(64),
                  chain_anchor: MOCK_CHAIN_ANCHOR_B64,
                  new_guardian_pubkey: '0x' + '1'.repeat(64),
                  new_guardian_endpoint: 'http://new-guardian.com',
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

      vi.mocked(chainAnchorFromBase64).mockReturnValueOnce({
        commitment: () => ({ toHex: () => '0x' + 'e'.repeat(64) }),
        free: vi.fn(),
        serialize: () => new Uint8Array([9, 9, 9]),
      } as never);

      await expect(multisig.syncProposals()).rejects.toThrow(
        'chain anchor does not match the block commitment bound into the tx_summary',
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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

      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
        toCommitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      } as any);

      await expect(
        multisig.createProposal(1, 'AQID', {
          proposalType: 'add_signer',
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
        summary: {
          toCommitment: () => ({
            toHex: () => '0x' + 'c'.repeat(64),
          }),
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      const proposal = await multisig.createP2idProposal('0xrecipient', '0xfaucet', 100n, { nonce: 1 });

      expect(proposal.metadata.description).toBe('Send 100 of asset 0xfaucet... to 0xrecipien...');
    });

    it('threads a private noteType into the request and wire metadata (issue #322)', async () => {
      const { executeForSummary, buildP2idTransactionRequest } = await import('./transaction.js');
      const { NoteType } = await import('@miden-sdk/miden-sdk');
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          toCommitment: () => ({
            toHex: () => '0x' + 'c'.repeat(64),
          }),
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      // Cleared so the nth-call assertion below counts from this proposal: the file
      // does not reset mocks between tests, and earlier cases in this describe block
      // leave calls on this builder.
      vi.mocked(buildP2idTransactionRequest).mockClear();

      const proposal = await multisig.createP2idProposal('0xrecipient', '0xfaucet', 100n, {
        nonce: 1,
        noteType: NoteType.Private,
      });

      // Propose path builds the private note. Asserted on the FIRST call: the flow
      // builds twice, create then rebuild, and the rebuild also passes Private, so a
      // plain toHaveBeenCalledWith is satisfied by the rebuild whatever the create
      // path did.
      expect(vi.mocked(buildP2idTransactionRequest)).toHaveBeenNthCalledWith(
        1,
        expect.any(String),
        '0xrecipient',
        '0xfaucet',
        100n,
        expect.objectContaining({ noteType: NoteType.Private }),
      );
      // ...and the rebuild-from-metadata path parses note_type back to Private.
      const lastCall = vi.mocked(buildP2idTransactionRequest).mock.calls.at(-1)!;
      expect(lastCall[4]).toMatchObject({ noteType: NoteType.Private });

      // The pushed wire metadata carries note_type so cosigners rebuild the
      // same private note at verification/execution.
      const pushBody = JSON.parse(mockFetch.mock.calls.at(-1)![1].body as string);
      expect(pushBody.delta_payload.metadata.note_type).toBe('private');

      expect(proposal.metadata.proposalType).toBe('p2id');
      expect((proposal.metadata as { noteType?: string }).noteType).toBe('private');
    });

    it('threads P2IDE reclaim/timelock heights into the request and wire metadata (issue #366)', async () => {
      const { executeForSummary, buildP2idTransactionRequest } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          toCommitment: () => ({
            toHex: () => '0x' + 'c'.repeat(64),
          }),
          serialize: () => new Uint8Array([1, 2, 3]),
        },
        anchor: createMockChainAnchor(),
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
            salt: '0x' + 'd'.repeat(64),
            recipient_id: '0xrecipient',
            faucet_id: '0xfaucet',
            amount: '100',
            reclaim_height: 12345,
            timelock_height: 700,
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

      vi.mocked(buildP2idTransactionRequest).mockClear();

      const proposal = await multisig.createP2idProposal('0xrecipient', '0xfaucet', 100n, {
        nonce: 1,
        reclaimHeight: 12345,
        timelockHeight: 700,
      });

      // Propose path builds the P2IDE note from the heights. Read call 1, not the last:
      // the flow builds twice, create then rebuild, and the rebuild passes the same
      // heights back from metadata, so the last call cannot witness the create path.
      const createCall = vi.mocked(buildP2idTransactionRequest).mock.calls[0]!;
      expect(createCall[4]).toMatchObject({ reclaimHeight: 12345, timelockHeight: 700 });

      // ...and the pushed wire metadata carries the heights so cosigners
      // rebuild the same P2IDE note at verification/execution.
      const pushBody = JSON.parse(mockFetch.mock.calls.at(-1)![1].body as string);
      expect(pushBody.delta_payload.metadata.reclaim_height).toBe(12345);
      expect(pushBody.delta_payload.metadata.timelock_height).toBe(700);

      expect(proposal.metadata.proposalType).toBe('p2id');
      expect(proposal.metadata).toMatchObject({ reclaimHeight: 12345, timelockHeight: 700 });
    });

    it('omits the heights from wire metadata for a plain P2ID send (pre-#366 shape)', async () => {
      const { executeForSummary } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          toCommitment: () => ({
            toHex: () => '0x' + 'c'.repeat(64),
          }),
          serialize: () => new Uint8Array([1, 2, 3]),
        },
        anchor: createMockChainAnchor(),
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
            salt: '0x' + 'd'.repeat(64),
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

      await multisig.createP2idProposal('0xrecipient', '0xfaucet', 100n, { nonce: 1 });

      const pushBody = JSON.parse(mockFetch.mock.calls.at(-1)![1].body as string);
      expect('reclaim_height' in pushBody.delta_payload.metadata).toBe(false);
      expect('timelock_height' in pushBody.delta_payload.metadata).toBe(false);
    });

    /// Options are forwarded wholesale so a note option added later cannot be
    /// dropped, which also means a structurally compatible object can carry
    /// `feeFaucetId` into a typed proposal. It is overridden rather than
    /// honoured: only the anchored block's faucet is reproducible by a rebuild,
    /// so a caller-chosen one would collect signatures and never execute.
    it('commits the chain fee faucet on a typed p2id proposal', async () => {
      const { executeForSummary } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          toCommitment: () => ({ toHex: () => '0x' + 'c'.repeat(64) }),
          serialize: () => new Uint8Array([1, 2, 3]),
        },
        anchor: createMockChainAnchor(),
      } as any);

      const multisig = createTestMultisig({
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      });
      vi.mocked(buildP2idTransactionRequest).mockClear();
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: {
            account_id: '0x' + 'a'.repeat(30),
            nonce: 1,
            prev_commitment: '0x' + 'b'.repeat(64),
            delta_payload: {
              tx_summary: { data: 'AQID' },
              signatures: [],
              metadata: {
                proposal_type: 'p2id',
                salt: '0x' + 'd'.repeat(64),
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
          },
          commitment: '0x' + 'c'.repeat(64),
        }),
      });

      // Passing a caller faucet exercises the override rather than merely documenting
      // it: this is the only typed path that spreads a caller options bag into the
      // builder, so reversing the spread would silently commit the caller's choice and
      // produce a proposal that collects signatures and can never be rebuilt.
      const optionsCarryingAFaucet = { nonce: 1, feeFaucetId: MOCK_OTHER_FEE_FAUCET_ID_HEX };
      await multisig.createP2idProposal(
        '0xrecipient',
        '0xfaucet',
        100n,
        optionsCarryingAFaucet,
      );

      // The nth form is load-bearing. This flow calls the builder twice -- once to
      // create and once to rebuild -- and the rebuild passes the chain faucet, so a
      // plain toHaveBeenCalledWith is satisfied by the rebuild no matter what the
      // create path committed.
      expect(vi.mocked(buildP2idTransactionRequest)).toHaveBeenNthCalledWith(
        1,
        expect.anything(),
        '0xrecipient',
        '0xfaucet',
        100n,
        expect.objectContaining({ feeFaucetId: MOCK_FEE_FAUCET_ID_HEX }),
      );
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
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
        summary: {
          toCommitment: () => ({
            toHex: () => '0x' + 'c'.repeat(64),
          }),
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
      await multisig.createChangeThresholdProposal(2, { nonce: 1 });

      expect(buildUpdateSignersTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        2,
        config.signerCommitments,
        { signatureScheme: 'ecdsa', feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
      );
    });
  });

  describe('createAddSignerProposal / createRemoveSignerProposal', () => {
    const config = {
      threshold: 2,
      signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64), '0x' + 'd'.repeat(64)],
      guardianCommitment: '0x' + 'c'.repeat(64),
    };

    const mockPushResponse = (proposalType: string) => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: {
            account_id: '0x' + 'a'.repeat(30),
            nonce: 1,
            prev_commitment: '0x' + 'b'.repeat(64),
            delta_payload: {
              tx_summary: { data: 'AQID' },
              signatures: [],
              metadata: { proposal_type: proposalType, description: '' },
            },
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
    };

    beforeEach(() => {
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          toCommitment: () => ({ toHex: () => '0x' + 'c'.repeat(64) }),
          serialize: () => new Uint8Array([1, 2, 3]),
        },
        anchor: createMockChainAnchor(),
      } as any);
    });

    it('add: passes newThreshold from the options bag and appends the commitment', async () => {
      mockPushResponse('add_signer');
      const newCommitment = '0x' + 'e'.repeat(64);

      const multisig = createTestMultisig(config);
      await multisig.createAddSignerProposal(newCommitment, { nonce: 1, newThreshold: 3 });

      expect(buildUpdateSignersTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        3,
        [...config.signerCommitments, newCommitment],
        { signatureScheme: mockSigner.scheme, feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
      );
    });

    it('add: defaults to the current threshold when newThreshold is omitted', async () => {
      mockPushResponse('add_signer');

      const multisig = createTestMultisig(config);
      await multisig.createAddSignerProposal('0x' + 'e'.repeat(64), { nonce: 1 });

      expect(buildUpdateSignersTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        config.threshold,
        expect.any(Array),
        { signatureScheme: mockSigner.scheme, feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
      );
    });

    it('remove: passes newThreshold from the options bag and drops the commitment', async () => {
      mockPushResponse('remove_signer');

      const multisig = createTestMultisig(config);
      await multisig.createRemoveSignerProposal('0x' + 'd'.repeat(64), { nonce: 1, newThreshold: 1 });

      expect(buildUpdateSignersTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        1,
        [config.signerCommitments[0], config.signerCommitments[1]],
        { signatureScheme: mockSigner.scheme, feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
      );
    });

    it('remove: defaults to min(current threshold, remaining signers) when newThreshold is omitted', async () => {
      mockPushResponse('remove_signer');

      const multisig = createTestMultisig(config);
      await multisig.createRemoveSignerProposal('0x' + 'd'.repeat(64), { nonce: 1 });

      expect(buildUpdateSignersTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        2,
        expect.any(Array),
        { signatureScheme: mockSigner.scheme, feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
      );
    });
  });

  describe('legacy positional-caller guard (issue #387)', () => {
    const config = {
      threshold: 1,
      signerCommitments: ['0x' + 'a'.repeat(64)],
      guardianCommitment: '0x' + 'c'.repeat(64),
    };

    it('rejects a legacy positional nonce where the options bag is expected', async () => {
      const multisig = createTestMultisig(config);

      await expect(
        multisig.createP2idProposal('0xrecipient', '0xfaucet', 100n, 1 as any),
      ).rejects.toThrow(/issue #387/);
      await expect(
        multisig.createConsumeNotesProposal(['0x1'], 1 as any),
      ).rejects.toThrow(/issue #387/);
      await expect(
        multisig.createAddSignerProposal('0x' + 'e'.repeat(64), 1 as any),
      ).rejects.toThrow(/issue #387/);
      await expect(
        multisig.createCustomProposal(new Uint8Array([1]), 'label', 1 as any),
      ).rejects.toThrow(/issue #387/);
    });

    it('rejects a legacy trailing argument after the options slot', async () => {
      const multisig = createTestMultisig(config);

      // Pre-#387 pattern: createP2idProposal(r, f, amount, nonceHole, { noteType })
      await expect(
        (multisig.createP2idProposal as any)('0xrecipient', '0xfaucet', 100n, undefined, {
          noteType: 'private',
        }),
      ).rejects.toThrow(/issue #387/);
      // Pre-#387 pattern: createAddSignerProposal(commitment, nonceHole, newThreshold)
      await expect(
        (multisig.createAddSignerProposal as any)('0x' + 'e'.repeat(64), undefined, 3),
      ).rejects.toThrow(/issue #387/);
    });
  });

  describe('createSwitchGuardianProposal', () => {
    it('should verify new endpoint commitment before creating proposal', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
        summary: {
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
        summary: {
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
        { signatureScheme: 'ecdsa', feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
      );
    });
  });

  describe('createUpdateProcedureThresholdProposal', () => {
    it('should create procedure-threshold update proposals', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          toCommitment: () => ({
            toHex: () => '0x' + 'c'.repeat(64),
          }),
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      const proposal = await multisig.createUpdateProcedureThresholdProposal('send_asset', 1, { nonce: 1 });

      expect(buildUpdateProcedureThresholdTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        'send_asset',
        1,
        { signatureScheme: 'falcon', feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
      );
      expect(proposal.metadata.proposalType).toBe('update_procedure_threshold');
      if (proposal.metadata.proposalType === 'update_procedure_threshold') {
        expect(proposal.metadata.targetProcedure).toBe('send_asset');
        expect(proposal.metadata.targetThreshold).toBe(1);
      }
    });

    it('passes the signer scheme to ECDSA procedure-threshold updates', async () => {
      vi.mocked(executeForSummary).mockResolvedValue({
        summary: {
          toCommitment: () => ({
            toHex: () => '0x' + 'c'.repeat(64),
          }),
          serialize: () => new Uint8Array([1, 2, 3]),
      },
        anchor: createMockChainAnchor(),
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      await multisig.createUpdateProcedureThresholdProposal('send_asset', 1, { nonce: 1 });

      expect(buildUpdateProcedureThresholdTransactionRequest).toHaveBeenCalledWith(
        mockWebClient,
        'send_asset',
        1,
        { signatureScheme: 'ecdsa', feeFaucetId: MOCK_FEE_FAUCET_ID_HEX },
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
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
        targetThreshold: 1,
        targetSignerCommitments: ['0x' + 'a'.repeat(64)],
        description: '',
      });

      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
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
                  salt: '0x' + 'd'.repeat(64),
                  chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
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
              saltHex: '0x' + 'd'.repeat(64),
              chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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

      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
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
            saltHex: '0x' + 'd'.repeat(64),
            chainAnchor: MOCK_CHAIN_ANCHOR_B64,
            targetThreshold: 1,
            targetSignerCommitments: ['0x' + 'a'.repeat(64)],
            description: '',
          },
        })
      );

      proposal.metadata = {
        proposalType: 'add_signer',
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
        targetThreshold: 2,
        targetSignerCommitments: ['0x' + 'a'.repeat(64)],
        description: '',
      };

      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
            saltHex: '0x' + 'd'.repeat(64),
            chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              saltHex: '0x' + 'd'.repeat(64),
              chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        2,
        expect.anything(),
        expect.anything(),
        expect.anything(),
      );
      // The advice payload now comes from the SDK, so the routing assertion is
      // the commitment each entry is keyed on: cosigner first, GUARDIAN ack
      // second. Swapping the two entries must not pass.
      const adviceCalls = vi.mocked(buildSignatureAdviceEntry).mock.calls;
      expect(adviceCalls[0][0].toHex()).toBe(config.signerCommitments[0]);
      expect(adviceCalls[1][0].toHex()).toBe(config.guardianCommitment);
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
            saltHex: '0x' + 'd'.repeat(64),
            chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
                salt: '0x' + 'd'.repeat(64),
                chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

    it('should fail when GUARDIAN ack signature is missing', async () => {
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        2,
        expect.anything(),
        expect.anything(),
        expect.anything(),
      );
      // The advice payload now comes from the SDK, so the routing assertion is
      // the commitment each entry is keyed on: cosigner first, GUARDIAN ack
      // second. Swapping the two entries must not pass.
      const adviceCalls = vi.mocked(buildSignatureAdviceEntry).mock.calls;
      expect(adviceCalls[0][0].toHex()).toBe(config.signerCommitments[0]);
      expect(adviceCalls[1][0].toHex()).toBe(config.guardianCommitment);
    });

    it('rejects a re-fetched tx_summary that is not the one the id was verified against', async () => {
      // Execution re-fetches the delta from GUARDIAN after the binding check, and
      // that summary decides both the advice keys and the auth arg the rebuild is
      // checked against. A
      // substituted one must not be trusted just because it arrived over the same
      // connection.
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
          proposalType: 'change_threshold',
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
          targetThreshold: 1,
          targetSignerCommitments: ['0x' + 'a'.repeat(64)],
          description: '',
        },
      });

      // Persistent rather than queued: the guard must fire before the ack push, so
      // no later response should be needed, and a stray earlier fetch must not
      // consume the delta this test is about.
      mockFetch.mockResolvedValue({
        ok: true,
        json: async () => ({
          account_id: multisig.accountId,
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            // A different summary than the locally held 'AQID' the binding check
            // ran against.
            tx_summary: { data: 'BAUG' },
            signatures: [],
            metadata: {
              proposal_type: 'change_threshold',
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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

      const substituted = {
        toCommitment: () => ({ toHex: () => '0x' + '9'.repeat(64) }),
        blockCommitment: () => ({ toHex: () => '0x' + 'b'.repeat(64) }),
        userParams: () => [0, 0, 0, 1, 2, 3, 4],
        serialize: () => new Uint8Array([4, 5, 6]),
      };
      // Keyed on the bytes rather than the call count, which the binding check
      // already consumed twice.
      mockTxSummaryDeserialize.mockImplementation((bytes: Uint8Array) =>
        bytes[0] === 4 ? substituted : DEFAULT_TX_SUMMARY,
      );

      await expect(multisig.executeProposal(proposalId)).rejects.toThrow(
        /tx_summary commitment 0x9{64} does not match the proposal id it belongs to/,
      );
      expect(mockWebClient.executeTransaction).not.toHaveBeenCalled();
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
            saltHex: '0x' + 'd'.repeat(64),
            chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
      );
      expect(vi.mocked(buildSignatureAdviceEntry)).toHaveBeenNthCalledWith(
        2,
        expect.anything(),
        expect.anything(),
        expect.anything(),
      );
      // The advice payload now comes from the SDK, so the routing assertion is
      // the commitment each entry is keyed on: cosigner first, GUARDIAN ack
      // second. Swapping the two entries must not pass.
      const adviceCalls = vi.mocked(buildSignatureAdviceEntry).mock.calls;
      expect(adviceCalls[0][0].toHex()).toBe(config.signerCommitments[0]);
      expect(adviceCalls[1][0].toHex()).toBe(config.guardianCommitment);
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
          saltHex: '0x' + 'd'.repeat(64),
          chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              proposal_type: 'b2agg',
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
              description: '',
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [],
          },
        }),
      });

      vi.useFakeTimers();
      try {
        const submission = multisig.submitTransaction('0x' + 'c'.repeat(64), {} as never);
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
        salt: '0x' + 'd'.repeat(64),
        chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
      vi.mocked(executeForSummaryAt).mockResolvedValueOnce({
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
            salt: '0x' + 'd'.repeat(64),
            chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
            saltHex: '0x' + 'd'.repeat(64),
            chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
        saltHex: '0x' + 'd'.repeat(64),
        chainAnchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
              salt: '0x' + 'd'.repeat(64),
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
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
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
              target_threshold: 3,
              signer_commitments: ['0xa', '0xb', '0xc'],
              salt: '0x' + 'a'.repeat(64),
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
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
              recipient_id: '0xrecipient',
              faucet_id: '0xfaucet',
              amount: '12345',
              salt: '0x' + 'a'.repeat(64),
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
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
              new_guardian_pubkey: '0xnewpubkey',
              new_guardian_endpoint: 'http://new-guardian.com',
              salt: '0x' + 'a'.repeat(64),
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

  describe('auth-arg derivation', () => {
    // `summaryAuthArg` and `feeAuthArg` are both mocked to 0xaa.., so any recorded
    // salt derives back to the signed auth arg and resolves — unless a test steers
    // `feeAuthArg` elsewhere, which is how the mismatch case is reached.
    const COMMITTED_SALT = '0x' + 'd'.repeat(64);

    const config = {
      threshold: 1,
      signerCommitments: ['0x' + 'a'.repeat(64)],
      guardianCommitment: '0x' + 'c'.repeat(64),
    };

    const ADD_SIGNER_METADATA = {
      proposal_type: 'add_signer',
      salt: '0x' + 'd'.repeat(64),
      chain_anchor: MOCK_CHAIN_ANCHOR_B64,
      target_threshold: 1,
      signer_commitments: ['0x' + 'a'.repeat(64)],
      description: '',
    };

    const proposalWithSalt = (
      salt: string,
      metadata: Record<string, unknown> = ADD_SIGNER_METADATA,
    ) => [
      {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'AQID' },
          signatures: [],
          metadata: {
            ...metadata,
            salt,
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

    const serveProposals = (proposals: unknown): void => {
      mockFetch.mockResolvedValueOnce({ ok: true, json: async () => ({ proposals }) });
    };
    const serveProposal = (salt: string, metadata?: Record<string, unknown>): void =>
      serveProposals(proposalWithSalt(salt, metadata));
    const saltHexOf = (options: unknown): string =>
      (options as { salt: { toHex: () => string } }).salt.toHex();
    type ThrownError = { code?: string; message: string; cause?: unknown };
    const caught = (op: Promise<unknown>): Promise<ThrownError | undefined> =>
      op.then(
        () => undefined,
        (thrown: unknown) => thrown as ThrownError,
      );
    // Seeds a fully-signed proposal straight into the cache. Execution reads the
    // cache, so this reaches the rebuild without going through sync.
    const NEW_GUARDIAN_PUBKEY = '0x' + '1'.repeat(64);
    const seedReadyProposal = (
      multisig: Multisig,
      proposalId: string,
      metadata: Record<string, unknown>,
    ): void => {
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
        metadata: { chainAnchor: MOCK_CHAIN_ANCHOR_B64, description: '', ...metadata },
      });
    };
    const seedSwitchGuardianProposal = (
      multisig: Multisig,
      proposalId: string,
      saltHex: string,
    ): void =>
      seedReadyProposal(multisig, proposalId, {
        proposalType: 'switch_guardian',
        newGuardianPubkey: NEW_GUARDIAN_PUBKEY,
        newGuardianEndpoint: 'http://new-guardian.com',
        saltHex,
      });
    const serveGuardianCommitment = (): void => {
      mockFetch.mockResolvedValue({
        ok: true,
        json: async () => ({ commitment: NEW_GUARDIAN_PUBKEY }),
      });
    };

    // Nothing can derive a commitment without the salt, so an absent one is
    // malformed metadata rather than an older shape to read around. `null` has to
    // land the same way as an omitted field: this SDK omits it, but GUARDIAN stores
    // metadata as opaque JSON, so a producer whose serializer writes nulls
    // round-trips `"salt": null` to every reader.
    it.each([
      ['an omitted salt', (m: { salt?: string | null }) => delete m.salt],
      ['an explicitly null salt', (m: { salt?: string | null }) => {
        m.salt = null;
      }],
    ])('rejects a proposal with %s', async (_label, unsetSalt) => {
      const multisig = createTestMultisig(config);
      const proposals = proposalWithSalt('unused');
      unsetSalt(proposals[0].delta_payload.metadata as { salt?: string | null });
      serveProposals(proposals);

      const error = await caught(multisig.syncProposals());
      expect(error?.code).toBe('proposal_salt_malformed');
      expect(error?.message).toContain('expected a hex string');
    });

    it('keeps a switch_guardian salt that resolves, with its faucet', async () => {
      // `verifyProposalMetadataBinding` does not rebuild this type, so the
      // derivation check is the only thing binding its salt — and the faucet has
      // to reach the rebuild, or it reproduces the auth arg with the
      // conversion-info preimage missing from the advice map.
      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);
      vi.mocked(buildUpdateGuardianTransactionRequest).mockClear();
      seedSwitchGuardianProposal(multisig, proposalId, COMMITTED_SALT);
      serveGuardianCommitment();

      await multisig.executeProposal(proposalId).catch(() => undefined);

      const [, , options] = vi.mocked(buildUpdateGuardianTransactionRequest).mock.calls[0]!;
      expect(saltHexOf(options)).toBe(COMMITTED_SALT);
      expect(options).toMatchObject({ feeFaucetId: MOCK_FEE_FAUCET_ID_HEX });
    });

    it('proposes with an auth arg the Rust SDK can rederive from salt_hex', async () => {
      // The cross-SDK invariant is "the auth arg is DERIVABLE from salt_hex", not
      // the stronger "the auth arg IS the salt": a bare arg aborts create-time
      // execution with ERR_FEE_CONVERSION_INFO_MISSING on a fee-charging chain,
      // so it cannot be the answer. Committing NATIVE conversion info keeps
      // derivability — the faucet is the chain's own, read from the anchored
      // block, so any SDK holding salt_hex and that block rederives the arg.
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockClear();
      vi.mocked(buildUpdateSignersTransactionRequest).mockClear();
      vi.mocked(executeForSummary).mockResolvedValueOnce({
        summary: {
          toCommitment: () => ({ toHex: () => '0x' + 'c'.repeat(64) }),
          serialize: () => new Uint8Array([1, 2, 3]),
        },
        anchor: createMockChainAnchor(),
      } as never);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          delta: {
            account_id: '0x' + 'a'.repeat(30),
            nonce: 1,
            prev_commitment: '0x' + 'b'.repeat(64),
            delta_payload: {
              tx_summary: { data: 'AQID' },
              signatures: [],
              metadata: { proposal_type: 'add_signer', description: '' },
            },
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

      await multisig.createAddSignerProposal('0x' + 'e'.repeat(64));

      // The first call is the propose build; a later one would be a rebuild.
      const options = vi.mocked(buildUpdateSignersTransactionRequest).mock.calls[0]![3];
      // The CHAIN's fee faucet specifically. An arbitrary faucet here would commit info no
      // other SDK could rederive, which is the case the old assertion was protecting.
      expect(options).toHaveProperty('feeFaucetId', MOCK_FEE_FAUCET_ID_HEX);
    });

    it('rebuilds with the conversion info the salt and anchor derive', async () => {
      const multisig = createTestMultisig(config);
      serveProposal('0x' + 'd'.repeat(64));

      await multisig.syncProposals();

      const options = vi.mocked(buildUpdateSignersTransactionRequest).mock.calls[0]![3];
      expect(options).toMatchObject({ feeFaucetId: MOCK_FEE_FAUCET_ID_HEX });
      // The recorded salt, not the commitment the summary carries. Rebuilding
      // from the commitment would reproduce the auth arg and so pass the
      // reconstruction check, with the conversion-info preimage missing.
      expect(saltHexOf(options)).toBe(COMMITTED_SALT);
    });

    it('rejects a proposal whose salt does not derive the signed auth arg', async () => {
      // The derivation check is what turns a wrong `salt_hex` into a named
      // failure here rather than an ERR_FEE_CONVERSION_INFO_MISSING abort at
      // proving, so steer the hash away from the signed arg to reach it.
      vi.mocked(feeAuthArg).mockReturnValueOnce({
        toHex: () => '0x' + 'f'.repeat(64),
      } as never);

      const multisig = createTestMultisig(config);
      serveProposal('0x' + 'd'.repeat(64));

      const error = await caught(multisig.syncProposals());

      // The proposal is dead at this point, so the message has to name what was
      // checked against what, and the recovery action.
      expect(error?.code).toBe('proposal_auth_arg_unresolvable');
      expect(error?.message).toMatch(
        /auth arg 0xa{64} is not the fee-conversion commitment to its metadata salt 0xd{64} under fee faucet 0xfee/,
      );
      expect(error?.message).toMatch(/Recreate the proposal and collect signatures again/);
    });

    it('rejects a metadata salt that is not hex', async () => {
      // The salt arrives as untrusted metadata, so a non-hex value should name
      // the offending field rather than surfacing as a decoding error from
      // whatever consumes the word later.
      const multisig = createTestMultisig(config);
      serveProposal('0xnotasalt');

      await expect(multisig.syncProposals()).rejects.toThrow(
        /malformed metadata salt '0xnotasalt': expected a 32-byte hex word/,
      );
    });

    it('verifies a committed proposal without touching the chain', async () => {
      // The faucet comes from the block the proposal's anchor pins, which is
      // both offline-capable and immune to the chain's answer changing after
      // the signatures were collected.
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockClear();
      serveProposal('0x' + 'd'.repeat(64));

      await multisig.syncProposals();

      expect(mockRpcGetBlockHeaderByNumber).not.toHaveBeenCalled();
      const options = vi.mocked(buildUpdateSignersTransactionRequest).mock.calls[0]![3];
      expect(options).toMatchObject({ feeFaucetId: MOCK_FEE_FAUCET_ID_HEX });
      expect(saltHexOf(options)).toBe(COMMITTED_SALT);
    });

    it('rebuilds with the anchor faucet even when the chain now reports another', async () => {
      // The test above cannot distinguish "read the anchor" from "hardcoded the
      // one faucet every mock returns", because the tip and the anchor agree by
      // default. Here they disagree, which is the whole cross-SDK claim: a
      // rebuilder derives the committed faucet from the anchor that travels with
      // the proposal, so a faucet change since the signatures were collected
      // cannot move it.
      const multisig = createTestMultisig(config);
      vi.mocked(chainAnchorFromBase64).mockReturnValueOnce({
        commitment: () => ({ toHex: () => '0x' + 'b'.repeat(64) }),
        blockHeader: () => ({
          feeFaucetId: () => MOCK_OTHER_FEE_FAUCET_ID,
          free: () => {},
        }),
        free: () => {},
        serialize: () => new Uint8Array([9, 9, 9]),
      } as never);
      serveProposal('0x' + 'd'.repeat(64));

      await multisig.syncProposals();

      const options = vi.mocked(buildUpdateSignersTransactionRequest).mock.calls[0]![3];
      expect(options).toMatchObject({ feeFaucetId: MOCK_OTHER_FEE_FAUCET_ID_HEX });
    });

    // Each proposal type reaches its own builder through its own arm of
    // `buildTransactionRequestFromMetadata`, so the faucet has to be threaded
    // through each one separately. Covering only `add_signer` left five arms
    // where dropping it would strand a committed proposal at rebuild time.
    const committedArms: Array<{
      type: string;
      metadata: Record<string, unknown>;
      builder: () => { mock: { calls: unknown[][] } };
      optionsIndex: number;
    }> = [
      {
        type: 'add_signer',
        metadata: ADD_SIGNER_METADATA,
        builder: () => vi.mocked(buildUpdateSignersTransactionRequest),
        optionsIndex: 3,
      },
      {
        type: 'update_procedure_threshold',
        metadata: {
          proposal_type: 'update_procedure_threshold',
          salt: '0x' + 'd'.repeat(64),
          chain_anchor: MOCK_CHAIN_ANCHOR_B64,
          target_threshold: 1,
          target_procedure: 'send_asset',
          description: '',
        },
        builder: () => vi.mocked(buildUpdateProcedureThresholdTransactionRequest),
        optionsIndex: 3,
      },
      {
        type: 'p2id',
        metadata: {
          proposal_type: 'p2id',
          salt: '0x' + 'd'.repeat(64),
          chain_anchor: MOCK_CHAIN_ANCHOR_B64,
          recipient_id: '0xrecipient',
          faucet_id: '0xfaucet',
          amount: '100',
          description: '',
        },
        builder: () => vi.mocked(buildP2idTransactionRequest),
        optionsIndex: 4,
      },
    ];

    for (const { type, metadata, builder, optionsIndex } of committedArms) {
      it(`threads the faucet through the ${type} rebuild`, async () => {
        const multisig = createTestMultisig(config);
        serveProposal(COMMITTED_SALT, metadata);

        await multisig.syncProposals();

        const options = builder().mock.calls.at(-1)![optionsIndex];
        expect(options).toMatchObject({ feeFaucetId: MOCK_FEE_FAUCET_ID_HEX });
        expect(saltHexOf(options)).toBe(COMMITTED_SALT);
      });
    }

    // The rebuild is what every cosigner runs, and the P2IDE heights are the one
    // input the arms above do not carry: their p2id fixture is a plain send. A
    // rebuild that drops them builds P2ID where the proposer built P2IDE, so the
    // commitment differs and a valid proposal is rejected at verification.
    it('threads the P2IDE heights back through the p2id rebuild', async () => {
      const multisig = createTestMultisig(config);
      serveProposal(COMMITTED_SALT, {
        proposal_type: 'p2id',
        salt: '0x' + 'd'.repeat(64),
        chain_anchor: MOCK_CHAIN_ANCHOR_B64,
        recipient_id: '0xrecipient',
        faucet_id: '0xfaucet',
        amount: '100',
        reclaim_height: 12345,
        timelock_height: 700,
        description: '',
      });

      await multisig.syncProposals();

      const options = vi.mocked(buildP2idTransactionRequest).mock.calls.at(-1)![4];
      expect(options).toMatchObject({ reclaimHeight: 12345, timelockHeight: 700 });
    });

    // The two `consume_notes` arms have no sync-path fixture — v1 is gated and v2
    // needs embedded notes whose ids match — so they are pinned against the
    // rebuild directly. Without this, dropping the faucet from either arm is
    // invisible.
    for (const { version, builder, noteIds } of [
      {
        version: 2,
        builder: () => vi.mocked(buildConsumeNotesTransactionRequestFromNotes),
        noteIds: [] as string[],
      },
      {
        version: 1,
        builder: () => vi.mocked(buildConsumeNotesTransactionRequest),
        noteIds: ['0xnote1'],
      },
    ]) {
      it(`threads the faucet through the consume_notes v${version} rebuild`, async () => {
        const multisig = createTestMultisig(config);

        await (multisig as any).buildTransactionRequestFromMetadata(
          {
            proposalType: 'consume_notes',
            saltHex: '0x' + 'd'.repeat(64),
            chainAnchor: MOCK_CHAIN_ANCHOR_B64,
            noteIds,
            notes: [],
            metadataVersion: version,
            description: '',
          },
          {
            salt: { toHex: () => COMMITTED_SALT },
            feeFaucetIdHex: MOCK_FEE_FAUCET_ID_HEX,
          },
        );

        const options = builder().mock.calls.at(-1)!.at(-1);
        expect(options).toMatchObject({ feeFaucetId: MOCK_FEE_FAUCET_ID_HEX });
        expect(saltHexOf(options)).toBe(COMMITTED_SALT);
      });
    }

    it('derives the auth arg on the execute path, not just on sync', async () => {
      // Execution re-fetches the delta and rebuilds from it, and until this test
      // every executed fixture happened to carry no salt — so the whole
      // derivation step could be skipped on the execute path without any test
      // noticing, while sync-time verification still passed.
      const multisig = createTestMultisig(config);
      const proposalId = '0x' + 'c'.repeat(64);
      vi.mocked(buildUpdateSignersTransactionRequest).mockClear();

      seedReadyProposal(multisig, proposalId, {
        proposalType: 'change_threshold',
        targetThreshold: 1,
        targetSignerCommitments: ['0x' + 'a'.repeat(64)],
        saltHex: COMMITTED_SALT,
      });

      mockFetch.mockResolvedValue({
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
              chain_anchor: MOCK_CHAIN_ANCHOR_B64,
              target_threshold: 1,
              signer_commitments: ['0x' + 'a'.repeat(64)],
              salt: COMMITTED_SALT,
            },
          },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'a'.repeat(64),
            cosigner_sigs: [],
          },
          // The ack push shares this response; execution rebuilds only after it
          // succeeds.
          ack_sig: '0x' + 'e'.repeat(128),
        }),
      });

      await multisig.executeProposal(proposalId).catch(() => undefined);

      // Two rebuilds: the binding check, then execution. Asserting the count
      // keeps this test honest — reading only the last call would silently fall
      // back to the binding check's rebuild if execution never got that far.
      const calls = vi.mocked(buildUpdateSignersTransactionRequest).mock.calls;
      expect(calls).toHaveLength(2);
      for (const call of calls) {
        expect(call[3]).toMatchObject({ feeFaucetId: MOCK_FEE_FAUCET_ID_HEX });
        expect(saltHexOf(call[3])).toBe(COMMITTED_SALT);
      }
    });

    // Well-formed hex is not necessarily a readable word — each limb has to be
    // below the field modulus, which `Word.fromHex` enforces and this suite's
    // mock does not. Driving the decode failure directly covers the branch a
    // shape-invalid salt never reaches, and pins that it is classified as a
    // salt fault rather than escaping as an opaque decode error.
    it('classifies an undecodable 32-byte salt as a salt fault', async () => {
      const multisig = createTestMultisig(config);
      const { Word } = await import('@miden-sdk/miden-sdk');
      serveProposal('0x' + 'f'.repeat(64));
      vi.mocked(Word.fromHex).mockImplementationOnce(() => {
        throw new Error('failed to convert to field element: value >= field modulus');
      });

      const error = await caught(multisig.syncProposals());
      expect(error?.code).toBe('proposal_salt_malformed');
      expect(error?.message).toContain('not a readable field element');
      expect((error?.cause as Error | undefined)?.message).toContain('field modulus');
    });

    // Zero-padding is what makes these dangerous: both normalize to the zero
    // word, a legal salt, so without an explicit check a proposal whose salt
    // field was written empty would be rebuilt against a salt nobody chose.
    it.each([
      ['an empty salt', ''],
      ['a prefix with no digits', '0x'],
      // `ensureHexPrefix` accepts either case, so a check written against `0x`
      // alone would let this one back through to the zero word.
      ['an uppercase prefix with no digits', '0X'],
    ])('rejects %s rather than reading it as the zero word', async (_label, saltHex) => {
      const multisig = createTestMultisig(config);
      serveProposal(saltHex);

      const error = await caught(multisig.syncProposals());
      expect(error?.code).toBe('proposal_salt_malformed');
      expect(error?.message).toContain('it is empty');
    });

    // The salt is cast, not validated, so its length is attacker-chosen. It has
    // to be rejected before `normalizeHexWord` copies and pads the whole thing.
    // The pair brackets the bound exactly: a full-length word is the longest
    // legal salt, so the cutoff cannot drift either way without failing here.
    it.each([
      ['an oversized salt', '0x' + 'a'.repeat(5000), 'got 5002 characters'],
      ['one character past a full word', '0x' + 'a'.repeat(65), 'got 67 characters'],
    ])('rejects %s on its length', async (_label, saltHex, expected) => {
      const multisig = createTestMultisig(config);
      serveProposal(saltHex);

      const error = await caught(multisig.syncProposals());
      expect(error?.code).toBe('proposal_salt_malformed');
      expect(error?.message).toContain(expected);
    });

    // The other half of the bound: a full-length word must survive the length
    // gate and be rejected, if at all, on its content.
    it('lets a full-length word past the length gate', async () => {
      const multisig = createTestMultisig(config);
      serveProposal('0x' + 'z'.repeat(64));

      const error = await caught(multisig.syncProposals());
      expect(error?.code).toBe('proposal_salt_malformed');
      expect(error?.message).toContain('expected a 32-byte hex word');
      expect(error?.message).not.toContain('characters');
    });

    it('zero-pads a short metadata salt rather than rejecting it', async () => {
      // Documented leniency, and load-bearing: a producer that emits hex without
      // padding to 32 bytes would otherwise have every proposal refused at sync.
      const multisig = createTestMultisig(config);
      serveProposal('0xd');

      await multisig.syncProposals();

      const options = vi.mocked(buildUpdateSignersTransactionRequest).mock.calls.at(-1)![3];
      expect(saltHexOf(options)).toBe(
        '0x' + 'd'.padStart(64, '0'),
      );
    });

    it('rejects a metadata salt longer than a word', async () => {
      // `normalizeHexWord` pads but never truncates, so an over-long salt
      // reaches the word decoder intact and must be named here rather than
      // surfacing as an opaque SDK decoding error.
      const multisig = createTestMultisig(config);
      serveProposal('0x' + 'aa'.repeat(33));

      await expect(multisig.syncProposals()).rejects.toThrow(
        /malformed metadata salt .*: expected a 32-byte hex word/,
      );
    });
  });

  describe('getFeeFaucetId', () => {
    const config = {
      threshold: 1,
      signerCommitments: ['0x' + 'a'.repeat(64)],
      guardianCommitment: '0x' + 'c'.repeat(64),
    };

    it('reads the block the client is synced to, on the configured endpoint', async () => {
      // Which block is asked for is the whole correctness question, not how many
      // times. `FeeParameters` is a per-block header field, and a request built
      // now is anchored at the synced block, so reading the tip instead would
      // commit a faucet from a block the proposal is not anchored at and no
      // rebuild could reproduce the auth arg.
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockClear();
      vi.mocked(Endpoint).mockClear();

      await multisig.getFeeFaucetId();

      expect(mockRpcGetBlockHeaderByNumber).toHaveBeenCalledWith(MOCK_SYNC_HEIGHT);
      expect(vi.mocked(Endpoint)).toHaveBeenCalledWith(MIDEN_RPC_ENDPOINT);
    });

    it('re-reads rather than caching, so a sync cannot leave it stale', async () => {
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockClear();

      await multisig.getFeeFaucetId();
      mockWebClient.getSyncHeight.mockResolvedValueOnce(MOCK_SYNC_HEIGHT + 1);
      mockRpcGetBlockHeaderByNumber.mockResolvedValueOnce({
        blockNum: () => MOCK_SYNC_HEIGHT + 1,
        feeFaucetId: () => MOCK_OTHER_FEE_FAUCET_ID,
        free: () => {},
      });

      await expect(multisig.getFeeFaucetId()).resolves.toBe(MOCK_OTHER_FEE_FAUCET_ID_HEX);
      expect(mockRpcGetBlockHeaderByNumber).toHaveBeenNthCalledWith(2, MOCK_SYNC_HEIGHT + 1);
    });

    it('names the endpoint when it fails, and stays usable afterwards', async () => {
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockClear();
      mockRpcGetBlockHeaderByNumber.mockRejectedValueOnce(new Error('connection refused'));

      await expect(multisig.getFeeFaucetId()).rejects.toThrow(
        /Failed to read the fee faucet from/,
      );

      // A transient failure must not make every later transaction unbuildable.
      await expect(multisig.getFeeFaucetId()).resolves.toBe(MOCK_FEE_FAUCET_ID_HEX);
    });

    it('surfaces the underlying RPC failure in the message, not only the cause', async () => {
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockClear();
      mockRpcGetBlockHeaderByNumber.mockRejectedValueOnce(new Error('connection refused'));

      await expect(multisig.getFeeFaucetId()).rejects.toThrow(/connection refused/);
    });

    it('retries a rate-limited read rather than surfacing it', async () => {
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockClear();
      mockRpcGetBlockHeaderByNumber.mockRejectedValueOnce(
        Object.assign(new Error('rate limited'), { code: 8 }),
      );

      await expect(multisig.getFeeFaucetId()).resolves.toBe(MOCK_FEE_FAUCET_ID_HEX);
      expect(mockRpcGetBlockHeaderByNumber).toHaveBeenCalledTimes(2);
    });

    it('refuses to publish a proposal whose anchor disagrees with the committed faucet', async () => {
      // A sync landing between the faucet read and the anchor capture would
      // otherwise mint a proposal that collects signatures and can never
      // execute: the auth arg commits one faucet, every rebuild derives the
      // other from the anchor. It has to fail at creation, not at execution.
      const multisig = createTestMultisig({
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'd'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      });
      mockRpcGetBlockHeaderByNumber.mockResolvedValueOnce({
        blockNum: () => MOCK_SYNC_HEIGHT,
        feeFaucetId: () => MOCK_OTHER_FEE_FAUCET_ID,
        free: () => {},
      });

      await expect(multisig.createChangeThresholdProposal(2)).rejects.toThrow(
        FeeFaucetAnchorMismatchError,
      );
    });

    it('carries a stable code and both faucets, so a caller can retry on the type', async () => {
      // The message tells the caller to retry, which only a human can act on
      // unless the condition is detectable. It is transient and the remedy is
      // mechanical, so it has to be reachable without matching prose.
      const multisig = createTestMultisig({
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'd'.repeat(64)],
        guardianCommitment: '0x' + 'c'.repeat(64),
      });
      mockRpcGetBlockHeaderByNumber.mockResolvedValueOnce({
        blockNum: () => MOCK_SYNC_HEIGHT,
        feeFaucetId: () => MOCK_OTHER_FEE_FAUCET_ID,
        free: () => {},
      });

      const error = await multisig.createChangeThresholdProposal(2).catch((e) => e);

      expect(error).toBeInstanceOf(FeeFaucetAnchorMismatchError);
      expect(error.code).toBe('fee_faucet_anchor_mismatch');
      expect(error.committedFeeFaucetIdHex).toBe(MOCK_OTHER_FEE_FAUCET_ID_HEX);
      expect(error.anchoredFeeFaucetIdHex).toBe(MOCK_FEE_FAUCET_ID_HEX);
    });

    it('rejects a header the node returned for a height other than the one asked for', async () => {
      // This reads through a raw RpcClient, which unlike miden-client's
      // verifying client does not check that the header it got back is the one
      // it requested. Only the anchored block's faucet is the right one to
      // commit, so a header from another height must not be trusted.
      const multisig = createTestMultisig(config);
      mockRpcGetBlockHeaderByNumber.mockResolvedValueOnce({
        blockNum: () => MOCK_SYNC_HEIGHT - 1,
        feeFaucetId: () => MOCK_FEE_FAUCET_ID,
        free: () => {},
      });

      await expect(multisig.getFeeFaucetId()).rejects.toThrow(
        /requested the block header at height 4242 but the node returned height 4241/,
      );
    });
  });
});
