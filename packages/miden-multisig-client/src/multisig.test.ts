import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Multisig } from './multisig.js';
import { PsmHttpClient, type Signer } from '@openzeppelin/psm-client';

const { mockRpcGetAccountDetails, mockAccountDeserialize, mockDetectConfig } = vi.hoisted(() => ({
  mockRpcGetAccountDetails: vi.fn(),
  mockAccountDeserialize: vi.fn(),
  mockDetectConfig: vi.fn(),
}));

// Mock the Miden SDK
vi.mock('@miden-sdk/miden-sdk', () => ({
  Account: {
    deserialize: mockAccountDeserialize,
  },
  AccountId: {
    fromHex: vi.fn((hex: string) => ({ toString: () => hex })),
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
  AdviceMap: vi.fn().mockImplementation(() => ({
    insert: vi.fn(),
  })),
  FeltArray: vi.fn().mockImplementation((arr: any[]) => arr),
  Rpo256: {
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
  buildUpdatePsmTransactionRequest: vi.fn().mockResolvedValue({
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

// Mock fetch for PSM client
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

function mockedAccount(commitmentHex: string, nonce = 0): any {
  return {
    commitment: () => ({
      toHex: () => commitmentHex,
    }),
    nonce: () => ({
      asInt: () => BigInt(nonce),
    }),
  };
}

describe('Multisig', () => {
  let psm: PsmHttpClient;
  let mockSigner: Signer;
  let mockAccount: any;
  let mockWebClient: any;

  beforeEach(() => {
    mockFetch.mockReset();
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
      psmEnabled: true,
      psmCommitment: '0x' + 'c'.repeat(64),
      vaultBalances: [],
      procedureThresholds: new Map(),
    });

    psm = new PsmHttpClient('http://localhost:3000');

    mockSigner = {
      commitment: '0x' + '1'.repeat(64),
      publicKey: '0x' + '2'.repeat(64),
      signRequest: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
      signCommitment: vi.fn().mockReturnValue('0x' + 'b'.repeat(128)),
    };

    psm.setSigner(mockSigner);

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
      getConsumableNotes: vi.fn().mockResolvedValue([]),
      syncState: vi.fn(),
      getAccount: vi.fn().mockResolvedValue(null),
      newAccount: vi.fn(),
    };
  });

  describe('constructor', () => {
    it('should create Multisig with account', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual(config.signerCommitments);
      expect(multisig.psmCommitment).toBe(config.psmCommitment);
      expect(multisig.account).toBe(mockAccount);
    });

    it('should create Multisig with explicit accountId override', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const accountId = '0x' + 'd'.repeat(30);
      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient, accountId);

      expect(multisig.account).toBe(mockAccount);
      expect(multisig.accountId).toBe(accountId);
    });
  });

  describe('accountId', () => {
    it('should return account ID from account', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      expect(multisig.accountId).toBe('0x' + 'a'.repeat(30));
    });

    it('should return provided account ID when constructor override is set', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const accountId = '0x' + 'e'.repeat(30);
      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient, accountId);
      expect(multisig.accountId).toBe(accountId);
    });
  });

  describe('signerCommitment', () => {
    it('should return signer commitment', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      expect(multisig.signerCommitment).toBe(mockSigner.commitment);
    });
  });

  describe('fetchState', () => {
    it('should fetch account state from PSM', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce({
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
      mockDetectConfig.mockReturnValueOnce({
        threshold: 2,
        numSigners: 2,
        signerCommitments: ['0x' + '1'.repeat(64), '0x' + '2'.repeat(64)],
        psmEnabled: true,
        psmCommitment: '0x' + 'd'.repeat(64),
        vaultBalances: [],
        procedureThresholds: new Map(),
      });

      await multisig.syncState();

      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual([
        '0x' + '1'.repeat(64),
        '0x' + '2'.repeat(64),
      ]);
      expect(multisig.psmCommitment).toBe('0x' + 'd'.repeat(64));
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
    });

    it('should overwrite local state when account is not found on-chain', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
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

    it('should throw when incoming state nonce is lower than local nonce', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
        mockSigner,
        mockWebClient,
        undefined,
        'https://rpc.devnet.miden.io'
      );

      mockWebClient.getAccount.mockResolvedValueOnce(mockedAccount('0x' + 'a'.repeat(64), 3));
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
        'incoming nonce 2 is not greater than local nonce 3'
      );
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
    });

    it('should throw when incoming state nonce equals local nonce but commitment differs', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
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
        'incoming nonce 2 is not greater than local nonce 2'
      );
      expect(mockWebClient.newAccount).not.toHaveBeenCalled();
    });
  });

  describe('verifyStateCommitment', () => {
    it('should pass when local and on-chain commitments match', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };
      mockWebClient.getAccount.mockResolvedValueOnce({
        commitment: () => ({
          toHex: () => '0x' + 'b'.repeat(64),
        }),
      });

      const multisigWithRpc = new Multisig(
        mockAccount,
        config,
        psm,
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
        psmCommitment: '0x' + 'c'.repeat(64),
      };
      mockWebClient.getAccount.mockResolvedValueOnce(null);

      const multisigWithRpc = new Multisig(
        mockAccount,
        config,
        psm,
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
        psmCommitment: '0x' + 'c'.repeat(64),
      };
      mockWebClient.getAccount.mockResolvedValueOnce({
        commitment: () => ({
          toHex: () => '0x' + 'f'.repeat(64),
        }),
      });
      mockRpcGetAccountDetails.mockResolvedValueOnce({
        commitment: () => ({
          toHex: () => '0x' + 'b'.repeat(64),
        }),
      });

      const multisigWithRpc = new Multisig(
        mockAccount,
        config,
        psm,
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

  describe('registerOnPsm', () => {
    it('should register account on PSM', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: true,
          message: 'Account configured',
          ack_pubkey: '0x' + 'd'.repeat(64),
        }),
      });

      await expect(multisig.registerOnPsm()).resolves.toBeUndefined();
    });

    it('should accept explicit initial state base64', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(
        mockAccount,
        config,
        psm,
        mockSigner,
        mockWebClient,
        '0x' + 'e'.repeat(30),
      );

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: true,
          message: 'Account configured',
        }),
      });

      await expect(multisig.registerOnPsm('base64initialstate')).resolves.toBeUndefined();
    });

    it('should throw on PSM registration failure', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          success: false,
          message: 'Account already exists',
        }),
      });

      await expect(multisig.registerOnPsm()).rejects.toThrow('Failed to register on PSM');
    });
  });

  describe('syncProposals', () => {
    it('should sync proposals from PSM', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

    it('should reject non-32-byte signer IDs from PSM proposals', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

    it('should reject duplicate normalized signer IDs from PSM proposals', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      expect(multisig.listProposals()).toEqual([]);
    });
  });

  describe('createProposal', () => {
    it('should create a new proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
  });

  describe('createP2idProposal', () => {
    it('should include the faucet asset in the proposal description', async () => {
      const { executeForSummary } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
  });

  describe('createSwitchPsmProposal', () => {
    it('should verify new endpoint commitment before creating proposal', async () => {
      const { executeForSummary } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      const newPsmPubkey = '0x' + '1'.repeat(64);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ pubkey: newPsmPubkey }),
      });

      const proposal = await multisig.createSwitchPsmProposal('http://new-psm.com', newPsmPubkey);

      expect(proposal.metadata?.proposalType).toBe('switch_psm');
      if (proposal.metadata?.proposalType === 'switch_psm') {
        expect(proposal.metadata.newPsmEndpoint).toBe('http://new-psm.com');
      }
      expect(mockFetch).toHaveBeenCalledWith(
        'http://new-psm.com/pubkey',
        expect.objectContaining({ method: 'GET' })
      );
    });

    it('should reject switch proposal when endpoint commitment does not match', async () => {
      const { executeForSummary } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ pubkey: '0x' + '2'.repeat(64) }),
      });

      await expect(
        multisig.createSwitchPsmProposal('http://new-psm.com', '0x' + '1'.repeat(64))
      ).rejects.toThrow('Refusing to use PSM endpoint');
    });
  });

  describe('createUpdateProcedureThresholdProposal', () => {
    it('should create procedure-threshold update proposals', async () => {
      const { buildUpdateProcedureThresholdTransactionRequest, executeForSummary } = await import('./transaction.js');
      vi.mocked(executeForSummary).mockResolvedValue({
        serialize: () => new Uint8Array([1, 2, 3]),
      } as any);

      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
      );
      expect(proposal.metadata.proposalType).toBe('update_procedure_threshold');
      if (proposal.metadata.proposalType === 'update_procedure_threshold') {
        expect(proposal.metadata.targetProcedure).toBe('send_asset');
        expect(proposal.metadata.targetThreshold).toBe(1);
      }
    });
  });

  describe('signProposal', () => {
    it('should sign a proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: [mockSigner.commitment],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => mockDelta,
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

    it('should reject proposals for a different account before signing', async () => {
      const config = {
        threshold: 1,
        signerCommitments: [mockSigner.commitment],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      const proposalId = '0x' + 'd'.repeat(64);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: '0x' + 'f'.repeat(30),
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
        }),
      });

      await expect(multisig.signProposal(proposalId)).rejects.toThrow(
        'Proposal is for a different account: 0x' + 'f'.repeat(30),
      );
      expect(mockSigner.signCommitment).not.toHaveBeenCalled();
    });
  });

  describe('exportProposal', () => {
    it('should export proposal for offline signing', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

    it('should throw if proposal not found', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 404,
        statusText: 'Not Found',
        text: async () => 'Proposal not found',
      });

      await expect(
        multisig.exportProposal('0x' + 'nonexistent'.repeat(5))
      ).rejects.toThrow('Proposal not found');
    });
  });

  describe('importProposal', () => {
    it('should reject imported signatures with non-32-byte signer IDs', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

      expect(() => multisig.importProposal(JSON.stringify(exported))).toThrow(
        'expected signerId as 32-byte hex',
      );
    });

    it('should reject offline signing if an imported proposal account is changed', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), mockSigner.commitment],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

      const proposal = multisig.importProposal(JSON.stringify(exported));
      proposal.accountId = '0x' + 'f'.repeat(30);

      expect(() => multisig.signProposalOffline(proposal.id)).toThrow(
        'Proposal is for a different account: 0x' + 'f'.repeat(30),
      );
      expect(mockSigner.signCommitment).not.toHaveBeenCalled();
    });
  });

  describe('executeProposal', () => {
    it('should throw if proposal not found locally', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      await expect(
        multisig.executeProposal('0x' + 'nonexistent'.repeat(5))
      ).rejects.toThrow('Proposal not found');
    });

    it('should throw if proposal is still pending', async () => {
      const config = {
        threshold: 2, // Need 2 signatures
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

    it('should fail when PSM ack signature is missing (selector ON)', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        'PSM did not return acknowledgment signature'
      );
    });

    it('should verify switch_psm endpoint commitment before execution', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      const proposalId = '0x' + 'c'.repeat(64);
      const newPsmPubkey = '0x' + '1'.repeat(64);

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
          proposalType: 'switch_psm',
          newPsmPubkey,
          newPsmEndpoint: 'http://new-psm.com',
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ pubkey: newPsmPubkey }),
      });
      mockWebClient.getAccount.mockResolvedValueOnce({
        serialize: () => new Uint8Array([1, 2, 3]),
      });
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ success: true, message: 'ok', ack_pubkey: '0x' + 'f'.repeat(64) }),
      });
      mockWebClient.executeTransaction.mockResolvedValueOnce({});
      mockWebClient.proveTransaction.mockResolvedValueOnce({});
      mockWebClient.submitProvenTransaction.mockResolvedValueOnce(1n);
      mockWebClient.applyTransaction.mockResolvedValueOnce(undefined);

      await expect(multisig.executeProposal(proposalId)).resolves.toBeUndefined();
      expect(mockWebClient.executeTransaction).toHaveBeenCalledTimes(1);
    });

    it('should reject switch_psm execution when endpoint commitment mismatches', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
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
          proposalType: 'switch_psm',
          newPsmPubkey: '0x' + '1'.repeat(64),
          newPsmEndpoint: 'http://new-psm.com',
          description: '',
        },
      });

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ pubkey: '0x' + '2'.repeat(64) }),
      });

      await expect(multisig.executeProposal(proposalId)).rejects.toThrow(
        'Refusing to use PSM endpoint'
      );
      expect(mockWebClient.executeTransaction).not.toHaveBeenCalled();
    });

    it('should reject duplicate normalized signer IDs during execution', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
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
          proposalType: 'switch_psm',
          newPsmPubkey: '0x' + '1'.repeat(64),
          newPsmEndpoint: 'http://new-psm.com',
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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
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
          proposalType: 'switch_psm',
          newPsmPubkey: '0x' + '1'.repeat(64),
          newPsmEndpoint: 'http://new-psm.com',
          description: '',
        },
      });

      await expect(multisig.executeProposal(proposalId)).rejects.toThrow(
        'Duplicate advice-map key detected',
      );
    });
  });

  describe('proposal metadata preservation', () => {
    it('should preserve local metadata when syncing proposals', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

    it('should use PSM metadata for new proposals from other signers', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

    it('should create switch_psm proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        proposalType: 'switch_psm',
        newPsmPubkey: '0xnewpubkey',
        newPsmEndpoint: 'http://new-psm.com',
        description: '',
      });

      expect(proposal.metadata?.proposalType).toBe('switch_psm');
    });
  });

  describe('proposal status transitions', () => {
    it('should transition from pending to ready when threshold met', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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
        psmCommitment: '0x' + 'd'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      expect(multisig.threshold).toBe(3);
    });

    it('should expose signerCommitments', () => {
      const signerCommitments = ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)];
      const config = {
        threshold: 2,
        signerCommitments,
        psmCommitment: '0x' + 'd'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      expect(multisig.signerCommitments).toEqual(signerCommitments);
    });

    it('should expose psmCommitment', () => {
      const psmCommitment = '0x' + 'psm'.repeat(20);
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment,
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      expect(multisig.psmCommitment).toBe(psmCommitment);
    });

    it('should expose account when provided', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'd'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);
      expect(multisig.account).toBe(mockAccount);
    });
  });

  describe('cross-client compatibility: sync with snake_case metadata', () => {
    it('should parse Rust client proposals with snake_case metadata', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      // Simulates a PSM response with canonical snake_case metadata
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
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

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

    it('should parse switch_psm proposal with snake_case fields', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner, mockWebClient);

      const switchPsmProposals = [
        {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: {
            tx_summary: { data: 'AQID' },
            signatures: [],
            metadata: {
              proposal_type: 'switch_psm',
              new_psm_pubkey: '0xnewpubkey',
              new_psm_endpoint: 'http://new-psm.com',
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
        json: async () => ({ proposals: switchPsmProposals }),
      });

      const proposals = await multisig.syncProposals();

      expect(proposals.length).toBe(1);
      expect(proposals[0].metadata?.proposalType).toBe('switch_psm');
      if (proposals[0].metadata?.proposalType === 'switch_psm') {
        expect(proposals[0].metadata.newPsmPubkey).toBe('0xnewpubkey');
        expect(proposals[0].metadata.newPsmEndpoint).toBe('http://new-psm.com');
      }
    });
  });
});
