import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { GuardianHttpClient, GuardianHttpError } from './http.js';
import type { Signer, ConfigureResponse, StateObject, DeltaObject, DeltaProposalResponse } from './types.js';

// Mock fetch globally
const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

// Mock signer for authenticated requests
const mockSigner: Signer = {
  commitment: '0x' + '1'.repeat(64),
  publicKey: '0x' + '2'.repeat(64),
  scheme: 'falcon',
  signAccountIdWithTimestamp: vi.fn().mockResolvedValue('0x' + 'a'.repeat(128)),
  signRequest: vi.fn().mockReturnValue('0x' + 'a'.repeat(128)),
  signCommitment: vi.fn().mockReturnValue('0x' + 'b'.repeat(128)),
};

describe('GuardianHttpClient', () => {
  let client: GuardianHttpClient;

  beforeEach(() => {
    client = new GuardianHttpClient('http://localhost:3000');
    mockFetch.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  describe('constructor', () => {
    it('should create client with baseUrl', () => {
      const c = new GuardianHttpClient('http://example.com:8080');
      expect(c).toBeInstanceOf(GuardianHttpClient);
    });
  });

  describe('getPubkey', () => {
    it('should return server public key', async () => {
      const expectedPubkey = '0x' + 'abc123'.repeat(10);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ commitment: expectedPubkey }),
      });

      const pubkey = await client.getPubkey();

      expect(pubkey).toEqual({ commitment: expectedPubkey, pubkey: undefined });
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

    it('should throw GuardianHttpError on non-ok response', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 500,
        statusText: 'Internal Server Error',
        text: async () => 'Server error message',
      });

      const error = await client.getPubkey().catch((e) => e);
      expect(error).toBeInstanceOf(GuardianHttpError);
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

  describe('getDeltaProposal', () => {
    it('should get a single delta proposal by commitment', async () => {
      client.setSigner(mockSigner);

      const serverProposal = {
        account_id: '0x' + 'a'.repeat(30),
        nonce: 1,
        prev_commitment: '0x' + 'b'.repeat(64),
        delta_payload: {
          tx_summary: { data: 'base64summary' },
          signatures: [],
          metadata: { proposal_type: 'change_threshold' as const, target_threshold: 2, signer_commitments: [] },
        },
        status: {
          status: 'pending' as const,
          timestamp: '2024-01-01T00:00:00Z',
          proposer_id: '0x' + 'c'.repeat(64),
          cosigner_sigs: [],
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => serverProposal,
      });

      const accountId = '0x' + 'a'.repeat(30);
      const commitment = '0x' + 'd'.repeat(64);
      const proposal = await client.getDeltaProposal(accountId, commitment);

      expect(proposal.accountId).toBe(accountId);
      expect(proposal.nonce).toBe(1);
      expect(mockFetch).toHaveBeenCalledWith(
        expect.stringContaining('/delta/proposal/single?'),
        expect.objectContaining({ method: 'GET' }),
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

  // --- lookupAccountByKeyCommitment -----

  describe('lookupAccountByKeyCommitment', () => {
    const keyCommitmentHex = '0x' + 'aa'.repeat(32);

    function makeLookupSigner() {
      return {
        commitment: keyCommitmentHex,
        publicKey: '0x' + 'bb'.repeat(897),
        scheme: 'falcon' as const,
        signAccountIdWithTimestamp: vi.fn(),
        signRequest: vi.fn(),
        signCommitment: vi.fn(),
        signLookupMessage: vi.fn().mockResolvedValue('0x' + 'cc'.repeat(762)),
      };
    }

    it('returns the parsed accounts list on a happy path', async () => {
      const signer = makeLookupSigner();
      client.setSigner(signer);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          accounts: [{ account_id: '0x7bfb0f38b0fafa103f86a805594170' }],
        }),
      });

      const result = await client.lookupAccountByKeyCommitment(keyCommitmentHex);

      expect(result.accounts).toHaveLength(1);
      expect(result.accounts[0].accountId).toBe('0x7bfb0f38b0fafa103f86a805594170');
      expect(signer.signLookupMessage).toHaveBeenCalledTimes(1);
      expect(signer.signLookupMessage).toHaveBeenCalledWith(
        keyCommitmentHex,
        expect.any(Number)
      );
    });

    it('treats an empty list as a successful response, not an error', async () => {
      const signer = makeLookupSigner();
      client.setSigner(signer);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ accounts: [] }),
      });

      const result = await client.lookupAccountByKeyCommitment(keyCommitmentHex);
      expect(result.accounts).toEqual([]);
    });

    it('returns multi-match results in order', async () => {
      const signer = makeLookupSigner();
      client.setSigner(signer);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          accounts: [
            { account_id: '0xaaa1' },
            { account_id: '0xbbb2' },
          ],
        }),
      });

      const result = await client.lookupAccountByKeyCommitment(keyCommitmentHex);
      expect(result.accounts.map((a) => a.accountId)).toEqual(['0xaaa1', '0xbbb2']);
    });

    it('attaches x-pubkey, x-signature, x-timestamp headers and uses /state/lookup with the query string', async () => {
      const signer = makeLookupSigner();
      client.setSigner(signer);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ accounts: [] }),
      });

      await client.lookupAccountByKeyCommitment(keyCommitmentHex);

      expect(mockFetch).toHaveBeenCalledTimes(1);
      const [url, init] = mockFetch.mock.calls[0];
      expect(url).toBe(
        `http://localhost:3000/state/lookup?key_commitment=${encodeURIComponent(keyCommitmentHex)}`
      );
      expect(init.method).toBe('GET');
      expect(init.headers['x-pubkey']).toBe(signer.publicKey);
      expect(init.headers['x-signature']).toMatch(/^0x/);
      expect(init.headers['x-timestamp']).toMatch(/^\d+$/);
    });

    it('throws a clear error when no signer is configured', async () => {
      // No setSigner() call.
      await expect(
        client.lookupAccountByKeyCommitment(keyCommitmentHex)
      ).rejects.toThrow(/No signer configured/);
      expect(mockFetch).not.toHaveBeenCalled();
    });

    it('throws a clear error when the signer does not implement signLookupMessage', async () => {
      // Default mockSigner from the outer describe does NOT implement
      // signLookupMessage — the HTTP client must reject up front rather than
      // sending a request the server will reject.
      client.setSigner(mockSigner);
      await expect(
        client.lookupAccountByKeyCommitment(keyCommitmentHex)
      ).rejects.toThrow(/signLookupMessage/);
      expect(mockFetch).not.toHaveBeenCalled();
    });

    it('propagates HTTP errors from the server through GuardianHttpError', async () => {
      const signer = makeLookupSigner();
      client.setSigner(signer);
      mockFetch.mockResolvedValueOnce({
        ok: false,
        status: 401,
        statusText: 'Unauthorized',
        text: async () => '{"code":"authentication_failed","error":"..."}',
      });

      const err = await client
        .lookupAccountByKeyCommitment(keyCommitmentHex)
        .catch((e) => e);
      expect(err).toBeInstanceOf(GuardianHttpError);
      expect(err.status).toBe(401);
    });

    it('rejects malformed server responses (missing accounts array)', async () => {
      const signer = makeLookupSigner();
      client.setSigner(signer);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({}),
      });

      await expect(
        client.lookupAccountByKeyCommitment(keyCommitmentHex)
      ).rejects.toThrow(/Malformed/);
    });
  });
});

describe('GuardianHttpError', () => {
  it('should create error with status, statusText, and body', () => {
    const error = new GuardianHttpError(404, 'Not Found', 'Resource not found');

    expect(error).toBeInstanceOf(Error);
    expect(error).toBeInstanceOf(GuardianHttpError);
    expect(error.status).toBe(404);
    expect(error.statusText).toBe('Not Found');
    expect(error.body).toBe('Resource not found');
    expect(error.message).toContain('404');
    expect(error.message).toContain('Not Found');
    expect(error.name).toBe('GuardianHttpError');
  });
});
