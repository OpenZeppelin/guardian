import { describe, it, expect, vi, beforeEach } from 'vitest';
import { Multisig } from './multisig.js';
import { PsmHttpClient, type Signer, type DeltaObject } from '@openzeppelin/psm-client';

// Mock the Miden SDK
vi.mock('@demox-labs/miden-sdk', () => ({
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
}));

// Mock transaction module
vi.mock('./transaction.js', () => ({
  executeForSummary: vi.fn(),
  buildUpdateSignersTransactionRequest: vi.fn().mockResolvedValue({
    request: {},
    salt: { toHex: () => '0x' + 'd'.repeat(64) },
    configHash: { toHex: () => '0x' + 'e'.repeat(64) },
  }),
  buildUpdateSignersTransactionRequestWithSignatures: vi.fn().mockResolvedValue({}),
  buildSignatureAdviceEntry: vi.fn().mockReturnValue({
    key: { toHex: () => '0x' + 'f'.repeat(64) },
    values: [1, 2, 3],
  }),
  signatureHexToBytes: vi.fn((hex: string) => new Uint8Array([0, 1, 2, 3])),
  normalizeHexWord: vi.fn((hex: string) => '0x' + hex.replace(/^0x/i, '').padStart(64, '0')),
}));

// Mock fetch for PSM client
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

describe('Multisig', () => {
  let psm: PsmHttpClient;
  let mockSigner: Signer;
  let mockAccount: any;

  beforeEach(() => {
    mockFetch.mockReset();

    psm = new PsmHttpClient('http://localhost:3000');

    mockSigner = {
      commitment: '0x' + '1'.repeat(64),
      publicKey: '0x' + '2'.repeat(64),
      signAccountId: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
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
  });

  describe('constructor', () => {
    it('should create Multisig with account', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      expect(multisig.threshold).toBe(2);
      expect(multisig.signerCommitments).toEqual(config.signerCommitments);
      expect(multisig.psmCommitment).toBe(config.psmCommitment);
      expect(multisig.account).toBe(mockAccount);
    });

    it('should create Multisig without account (loaded)', () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const accountId = '0x' + 'd'.repeat(30);
      const multisig = new Multisig(null, config, psm, mockSigner, accountId);

      expect(multisig.account).toBeNull();
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

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);
      expect(multisig.accountId).toBe('0x' + 'a'.repeat(30));
    });

    it('should return provided account ID for loaded multisig', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const accountId = '0x' + 'e'.repeat(30);
      const multisig = new Multisig(null, config, psm, mockSigner, accountId);
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

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);
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

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

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

  describe('registerOnPsm', () => {
    it('should register account on PSM', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

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

    it('should throw when no account and no initial state', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(null, config, psm, mockSigner, '0x' + 'e'.repeat(30));

      await expect(multisig.registerOnPsm()).rejects.toThrow('Cannot register on PSM');
    });

    it('should accept initial state base64', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(null, config, psm, mockSigner, '0x' + 'e'.repeat(30));

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

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

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

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      const mockProposals: DeltaObject[] = [
        {
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
            cosigner_sigs: [
              {
                signer_id: '0x' + 'd'.repeat(64),
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
      expect(proposals[0].status.type).toBe('pending');
    });

    it('should return ready status when enough signatures', async () => {
      const config = {
        threshold: 1, // Only 1 signature needed
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      const mockProposals: DeltaObject[] = [
        {
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
            cosigner_sigs: [
              {
                signer_id: '0x' + 'd'.repeat(64),
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

      expect(proposals[0].status.type).toBe('ready');
    });
  });

  describe('listProposals', () => {
    it('should return empty list initially', () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);
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

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      const mockDelta: DeltaObject = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'base64summary' },
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

      const proposal = await multisig.createProposal(1, 'AQID');

      expect(proposal.nonce).toBe(1);
      expect(proposal.id).toBe('0x' + 'd'.repeat(64));
    });
  });

  describe('signProposal', () => {
    it('should sign a proposal', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      // First create a proposal
      const mockDelta: DeltaObject = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'base64summary' },
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

      await multisig.createProposal(1, 'AQID');

      // Now sign it
      const signedDelta: DeltaObject = {
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
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => signedDelta,
      });

      const proposalId = '0x' + 'd'.repeat(64);
      const signedProposal = await multisig.signProposal(proposalId);

      expect(mockSigner.signCommitment).toHaveBeenCalledWith(proposalId);
      expect(signedProposal.signatures.length).toBe(1);
    });
  });

  describe('exportProposal', () => {
    it('should export proposal for offline signing', async () => {
      const config = {
        threshold: 2,
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      const mockProposals: DeltaObject[] = [
        {
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
            cosigner_sigs: [
              {
                signer_id: '0x' + 'd'.repeat(64),
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

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals: [] }),
      });

      await expect(
        multisig.exportProposal('0x' + 'nonexistent'.repeat(5))
      ).rejects.toThrow('Proposal not found');
    });
  });

  describe('executeProposal', () => {
    it('should throw if proposal not found locally', async () => {
      const config = {
        threshold: 1,
        signerCommitments: ['0x' + 'a'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);
      const webClient = {} as any;

      await expect(
        multisig.executeProposal('0x' + 'nonexistent'.repeat(5), webClient)
      ).rejects.toThrow('Proposal not found');
    });

    it('should throw if proposal is still pending', async () => {
      const config = {
        threshold: 2, // Need 2 signatures
        signerCommitments: ['0x' + 'a'.repeat(64), '0x' + 'b'.repeat(64)],
        psmCommitment: '0x' + 'c'.repeat(64),
      };

      const multisig = new Multisig(mockAccount, config, psm, mockSigner);

      // Sync with pending proposal (only 1 signature)
      const mockProposals: DeltaObject[] = [
        {
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
            cosigner_sigs: [
              {
                signer_id: '0x' + 'd'.repeat(64),
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

      const webClient = {} as any;

      // Proposal ID is mocked to return 'c'.repeat(64)
      await expect(
        multisig.executeProposal('0x' + 'c'.repeat(64), webClient)
      ).rejects.toThrow('not ready for execution');
    });
  });
});
