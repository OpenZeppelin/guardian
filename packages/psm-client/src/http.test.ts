import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { PsmHttpClient, PsmHttpError } from './http.js';
import type { Signer, ConfigureResponse, StateObject, DeltaObject, DeltaProposalResponse } from './types.js';

// Mock fetch globally
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

// Mock signer for authenticated requests
const mockSigner: Signer = {
  commitment: '0x' + '1'.repeat(64),
  publicKey: '0x' + '2'.repeat(64),
  signAccountId: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
  signCommitment: vi.fn().mockReturnValue('0x' + 'b'.repeat(128)),
};

describe('PsmHttpClient', () => {
  let client: PsmHttpClient;

  beforeEach(() => {
    client = new PsmHttpClient('http://localhost:3000');
    mockFetch.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('constructor', () => {
    it('should create client with baseUrl', () => {
      const c = new PsmHttpClient('http://example.com:8080');
      expect(c).toBeInstanceOf(PsmHttpClient);
    });
  });

  describe('getPubkey', () => {
    it('should return server public key', async () => {
      const expectedPubkey = '0x' + 'abc123'.repeat(10);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ pubkey: expectedPubkey }),
      });

      const pubkey = await client.getPubkey();

      expect(pubkey).toBe(expectedPubkey);
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/pubkey',
        expect.objectContaining({
          method: 'GET',
          headers: expect.objectContaining({
            'Content-Type': 'application/json',
          }),
        })
      );
    });

    it('should throw PsmHttpError on non-ok response', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 500,
        statusText: 'Internal Server Error',
        text: async () => 'Server error message',
      });

      const error = await client.getPubkey().catch((e) => e);
      expect(error).toBeInstanceOf(PsmHttpError);
      expect(error.status).toBe(500);
      expect(error.statusText).toBe('Internal Server Error');
    });
  });

  describe('configure', () => {
    it('should configure account with authentication', async () => {
      client.setSigner(mockSigner);

      const configureResponse: ConfigureResponse = {
        success: true,
        message: 'Account configured',
        ack_pubkey: '0x' + 'c'.repeat(64),
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => configureResponse,
      });

      const request = {
        account_id: '0x' + 'd'.repeat(30),
        auth: {
          MidenFalconRpo: {
            cosigner_commitments: ['0x' + 'e'.repeat(64)],
          },
        },
        initial_state: { data: 'base64data', account_id: '0x' + 'd'.repeat(30) },
        storage_type: 'Filesystem' as const,
      };

      const response = await client.configure(request);

      expect(response).toEqual(configureResponse);
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/configure',
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify(request),
          headers: expect.objectContaining({
            'x-pubkey': mockSigner.publicKey,
            'x-signature': expect.any(String),
          }),
        })
      );
    });

    it('should throw error when no signer configured', async () => {
      const request = {
        account_id: '0x' + 'd'.repeat(30),
        auth: { MidenFalconRpo: { cosigner_commitments: [] } },
        initial_state: { data: 'base64data', account_id: '0x' + 'd'.repeat(30) },
        storage_type: 'Filesystem' as const,
      };

      await expect(client.configure(request)).rejects.toThrow('No signer configured');
    });
  });

  describe('getState', () => {
    it('should get account state with authentication', async () => {
      client.setSigner(mockSigner);

      const stateObject: StateObject = {
        account_id: '0x' + 'a'.repeat(30),
        commitment: '0x' + 'b'.repeat(64),
        state_json: { data: 'base64state' },
        created_at: '2024-01-01T00:00:00Z',
        updated_at: '2024-01-02T00:00:00Z',
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => stateObject,
      });

      const accountId = '0x' + 'a'.repeat(30);
      const state = await client.getState(accountId);

      expect(state).toEqual(stateObject);
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('/state?'),
        expect.objectContaining({
          method: 'GET',
          headers: expect.objectContaining({
            'x-pubkey': mockSigner.publicKey,
          }),
        })
      );
    });
  });

  describe('getDeltaProposals', () => {
    it('should get delta proposals for account', async () => {
      client.setSigner(mockSigner);

      const proposals: DeltaObject[] = [
        {
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
        },
      ];

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ proposals }),
      });

      const accountId = '0x' + 'a'.repeat(30);
      const result = await client.getDeltaProposals(accountId);

      expect(result).toEqual(proposals);
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('/delta/proposal?'),
        expect.objectContaining({ method: 'GET' })
      );
    });
  });

  describe('pushDeltaProposal', () => {
    it('should push a new delta proposal', async () => {
      client.setSigner(mockSigner);

      const proposalResponse: DeltaProposalResponse = {
        delta: {
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
        },
        commitment: '0x' + 'd'.repeat(64),
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => proposalResponse,
      });

      const request = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        delta_payload: {
          tx_summary: { data: 'base64summary' },
          signatures: [],
        },
      };

      const result = await client.pushDeltaProposal(request);

      expect(result).toEqual(proposalResponse);
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/delta/proposal',
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify(request),
        })
      );
    });
  });

  describe('signDeltaProposal', () => {
    it('should sign a delta proposal', async () => {
      client.setSigner(mockSigner);

      const delta: DeltaObject = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'base64summary' },
          signatures: [{ signer_id: '0x' + 'c'.repeat(64), signature: { scheme: 'falcon', signature: '0x' + 'd'.repeat(128) } }],
        },
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [
            {
              signer_id: '0x' + 'c'.repeat(64),
              signature: { scheme: 'falcon', signature: '0x' + 'd'.repeat(128) },
              timestamp: '2024-01-01T00:00:00Z',
            },
          ],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => delta,
      });

      const request = {
        account_id: '0x' + 'a'.repeat(30),
        commitment: '0x' + 'e'.repeat(64),
        signature: { scheme: 'falcon' as const, signature: '0x' + 'd'.repeat(128) },
      };

      const result = await client.signDeltaProposal(request);

      expect(result).toEqual(delta);
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/delta/proposal',
        expect.objectContaining({
          method: 'PUT',
          body: JSON.stringify(request),
        })
      );
    });
  });

  describe('pushDelta', () => {
    it('should push a delta for execution', async () => {
      client.setSigner(mockSigner);

      const resultDelta: DeltaObject = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'base64summary' },
          signatures: [],
        },
        ack_sig: '0x' + 'f'.repeat(128),
        status: {
          status: 'candidate',
          timestamp: '2024-01-01T00:00:00Z',
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => resultDelta,
      });

      const executionDelta = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: { data: 'base64summary' },
        status: {
          status: 'pending' as const,
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      const result = await client.pushDelta(executionDelta);

      expect(result).toEqual(resultDelta);
      expect(result.ack_sig).toBe('0x' + 'f'.repeat(128));
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/delta',
        expect.objectContaining({
          method: 'POST',
        })
      );
    });
  });

  describe('getDelta', () => {
    it('should get a specific delta by nonce', async () => {
      client.setSigner(mockSigner);

      const delta: DeltaObject = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 5,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'base64summary' },
          signatures: [],
        },
        status: {
          status: 'canonical',
          timestamp: '2024-01-01T00:00:00Z',
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => delta,
      });

      const result = await client.getDelta('0x' + 'a'.repeat(30), 5);

      expect(result).toEqual(delta);
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('/delta?'),
        expect.objectContaining({ method: 'GET' })
      );
    });
  });

  describe('getDeltaSince', () => {
    it('should get merged delta since a nonce', async () => {
      client.setSigner(mockSigner);

      const delta: DeltaObject = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 10,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'base64mergeddata' },
          signatures: [],
        },
        status: {
          status: 'canonical',
          timestamp: '2024-01-01T00:00:00Z',
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => delta,
      });

      const result = await client.getDeltaSince('0x' + 'a'.repeat(30), 5);

      expect(result).toEqual(delta);
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('/delta/since?'),
        expect.objectContaining({ method: 'GET' })
      );
    });
  });
});

describe('PsmHttpError', () => {
  it('should create error with status, statusText, and body', () => {
    const error = new PsmHttpError(404, 'Not Found', 'Resource not found');

    expect(error).toBeInstanceOf(Error);
    expect(error).toBeInstanceOf(PsmHttpError);
    expect(error.status).toBe(404);
    expect(error.statusText).toBe('Not Found');
    expect(error.body).toBe('Resource not found');
    expect(error.message).toContain('404');
    expect(error.message).toContain('Not Found');
    expect(error.name).toBe('PsmHttpError');
  });
});
