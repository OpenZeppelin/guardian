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

      // Server returns snake_case
      const serverResponse = {
        success: true,
        message: 'Account configured',
        ack_pubkey: '0x' + 'c'.repeat(64),
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => serverResponse,
      });

      // Client API uses camelCase
      const request = {
        accountId: '0x' + 'd'.repeat(30),
        auth: {
          MidenFalconRpo: {
            cosigner_commitments: ['0x' + 'e'.repeat(64)],
          },
        },
        initialState: { data: 'base64data', accountId: '0x' + 'd'.repeat(30) },
        storageType: 'Filesystem' as const,
      };

      const response = await client.configure(request);

      // Client returns camelCase
      const expectedResponse: ConfigureResponse = {
        success: true,
        message: 'Account configured',
        ackPubkey: '0x' + 'c'.repeat(64),
      };
      expect(response).toEqual(expectedResponse);

      // Wire format is snake_case
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/configure',
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify({
            account_id: '0x' + 'd'.repeat(30),
            auth: { MidenFalconRpo: { cosigner_commitments: ['0x' + 'e'.repeat(64)] } },
            initial_state: { data: 'base64data', account_id: '0x' + 'd'.repeat(30) },
            storage_type: 'Filesystem',
          }),
          headers: expect.objectContaining({
            'x-pubkey': mockSigner.publicKey,
            'x-signature': expect.any(String),
          }),
        })
      );
    });

    it('should throw error when no signer configured', async () => {
      const request = {
        accountId: '0x' + 'd'.repeat(30),
        auth: { MidenFalconRpo: { cosigner_commitments: [] } },
        initialState: { data: 'base64data', accountId: '0x' + 'd'.repeat(30) },
        storageType: 'Filesystem' as const,
      };

      await expect(client.configure(request)).rejects.toThrow('No signer configured');
    });
  });

  describe('getState', () => {
    it('should get account state with authentication', async () => {
      client.setSigner(mockSigner);

      // Server returns snake_case
      const serverState = {
        account_id: '0x' + 'a'.repeat(30),
        commitment: '0x' + 'b'.repeat(64),
        state_json: { data: 'base64state' },
        created_at: '2024-01-01T00:00:00Z',
        updated_at: '2024-01-02T00:00:00Z',
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => serverState,
      });

      const accountId = '0x' + 'a'.repeat(30);
      const state = await client.getState(accountId);

      // Client returns camelCase
      const expectedState: StateObject = {
        accountId: '0x' + 'a'.repeat(30),
        commitment: '0x' + 'b'.repeat(64),
        stateJson: { data: 'base64state' },
        createdAt: '2024-01-01T00:00:00Z',
        updatedAt: '2024-01-02T00:00:00Z',
      };

      expect(state).toEqual(expectedState);
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

      // Server returns snake_case
      const serverProposals = [
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
        json: async () => ({ proposals: serverProposals }),
      });

      const accountId = '0x' + 'a'.repeat(30);
      const result = await client.getDeltaProposals(accountId);

      // Client returns camelCase
      const expectedProposals: DeltaObject[] = [
        {
          accountId: '0x' + 'a'.repeat(30),
          nonce: 1,
          prevCommitment: '0x' + 'b'.repeat(64),
          newCommitment: undefined,
          deltaPayload: {
            txSummary: { data: 'base64summary' },
            signatures: [],
            metadata: undefined,
          },
          ackSig: undefined,
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposerId: '0x' + 'c'.repeat(64),
            cosignerSigs: [],
          },
        },
      ];

      expect(result).toEqual(expectedProposals);
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('/delta/proposal?'),
        expect.objectContaining({ method: 'GET' })
      );
    });
  });

  describe('pushDeltaProposal', () => {
    it('should push a new delta proposal', async () => {
      client.setSigner(mockSigner);

      // Server returns snake_case
      const serverResponse = {
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
        json: async () => serverResponse,
      });

      // Client API uses camelCase
      const request = {
        accountId: '0x' + 'a'.repeat(30),
        nonce: 1,
        deltaPayload: {
          txSummary: { data: 'base64summary' },
          signatures: [],
        },
      };

      const result = await client.pushDeltaProposal(request);

      // Client returns camelCase
      const expectedResponse: DeltaProposalResponse = {
        delta: {
          accountId: '0x' + 'a'.repeat(30),
          nonce: 1,
          prevCommitment: '0x' + 'b'.repeat(64),
          newCommitment: undefined,
          deltaPayload: {
            txSummary: { data: 'base64summary' },
            signatures: [],
            metadata: undefined,
          },
          ackSig: undefined,
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposerId: '0x' + 'c'.repeat(64),
            cosignerSigs: [],
          },
        },
        commitment: '0x' + 'd'.repeat(64),
      };

      expect(result).toEqual(expectedResponse);

      // Wire format is snake_case
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/delta/proposal',
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify({
            account_id: '0x' + 'a'.repeat(30),
            nonce: 1,
            delta_payload: {
              tx_summary: { data: 'base64summary' },
              signatures: [],
            },
          }),
        })
      );
    });
  });

  describe('signDeltaProposal', () => {
    it('should sign a delta proposal', async () => {
      client.setSigner(mockSigner);

      // Server returns snake_case
      const serverDelta = {
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
        json: async () => serverDelta,
      });

      // Client API uses camelCase
      const request = {
        accountId: '0x' + 'a'.repeat(30),
        commitment: '0x' + 'e'.repeat(64),
        signature: { scheme: 'falcon' as const, signature: '0x' + 'd'.repeat(128) },
      };

      const result = await client.signDeltaProposal(request);

      // Client returns camelCase
      const expectedDelta: DeltaObject = {
        accountId: '0x' + 'a'.repeat(30),
        nonce: 1,
        prevCommitment: '0x' + 'b'.repeat(64),
        newCommitment: undefined,
        deltaPayload: {
          txSummary: { data: 'base64summary' },
          signatures: [{ signerId: '0x' + 'c'.repeat(64), signature: { scheme: 'falcon', signature: '0x' + 'd'.repeat(128) } }],
          metadata: undefined,
        },
        ackSig: undefined,
        status: {
          status: 'pending',
          timestamp: '2024-01-01T00:00:00Z',
          proposerId: '0x' + 'c'.repeat(64),
          cosignerSigs: [
            {
              signerId: '0x' + 'c'.repeat(64),
              signature: { scheme: 'falcon', signature: '0x' + 'd'.repeat(128) },
              timestamp: '2024-01-01T00:00:00Z',
            },
          ],
        },
      };

      expect(result).toEqual(expectedDelta);

      // Wire format is snake_case
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/delta/proposal',
        expect.objectContaining({
          method: 'PUT',
          body: JSON.stringify({
            account_id: '0x' + 'a'.repeat(30),
            commitment: '0x' + 'e'.repeat(64),
            signature: { scheme: 'falcon', signature: '0x' + 'd'.repeat(128) },
          }),
        })
      );
    });
  });

  describe('pushDelta', () => {
    it('should push a delta for execution and return ack signature', async () => {
      client.setSigner(mockSigner);

      // Server returns snake_case - execution delta response has raw delta_payload
      const serverResponse = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        new_commitment: '0x' + 'd'.repeat(64),
        delta_payload: { data: 'base64summary' },
        ack_sig: '0x' + 'f'.repeat(128),
        status: {
          status: 'candidate',
          timestamp: '2024-01-01T00:00:00Z',
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => serverResponse,
      });

      // Client API uses camelCase
      const executionDelta = {
        accountId: '0x' + 'a'.repeat(30),
        nonce: 1,
        prevCommitment: '0x' + 'b'.repeat(64),
        deltaPayload: { data: 'base64summary' },
        status: {
          status: 'pending' as const,
          timestamp: '2024-01-01T00:00:00Z',
          proposerId: '0x' + 'c'.repeat(64),
          cosignerSigs: [],
        },
      };

      const result = await client.pushDelta(executionDelta);

      // PushDeltaResponse only includes essential fields for execution
      expect(result.accountId).toBe('0x' + 'a'.repeat(30));
      expect(result.nonce).toBe(1);
      expect(result.newCommitment).toBe('0x' + 'd'.repeat(64));
      expect(result.ackSig).toBe('0x' + 'f'.repeat(128));

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

      // Server returns snake_case
      const serverDelta = {
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
        json: async () => serverDelta,
      });

      const result = await client.getDelta('0x' + 'a'.repeat(30), 5);

      // Client returns camelCase
      expect(result.accountId).toBe('0x' + 'a'.repeat(30));
      expect(result.nonce).toBe(5);
      expect(result.prevCommitment).toBe('0x' + 'b'.repeat(64));
      expect(result.status.status).toBe('canonical');

      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('/delta?'),
        expect.objectContaining({ method: 'GET' })
      );
    });
  });

  describe('getDeltaSince', () => {
    it('should get merged delta since a nonce', async () => {
      client.setSigner(mockSigner);

      // Server returns snake_case
      const serverDelta = {
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
        json: async () => serverDelta,
      });

      const result = await client.getDeltaSince('0x' + 'a'.repeat(30), 5);

      // Client returns camelCase
      expect(result.accountId).toBe('0x' + 'a'.repeat(30));
      expect(result.nonce).toBe(10);
      expect(result.deltaPayload.txSummary.data).toBe('base64mergeddata');

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
