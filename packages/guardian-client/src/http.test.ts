import { readFileSync } from 'node:fs';
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
        headers: new Headers(),
        status: 500,
        statusText: 'Internal Server Error',
        text: async () => 'Server error message',
      });

      const error = await client.getPubkey().catch((e) => e);
      expect(error).toBeInstanceOf(GuardianHttpError);
      expect(error.status).toBe(500);
      expect(error.statusText).toBe('Internal Server Error');
      // Non-JSON body: no typed envelope fields.
      expect(error.code).toBeNull();
      expect(error.releasedAt).toBeNull();
    });

    it('exposes code and released_at from a GUARDIAN_ACCOUNT_RELEASED envelope', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        headers: new Headers(),
        status: 409,
        statusText: 'Conflict',
        text: async () =>
          JSON.stringify({
            code: 'GUARDIAN_ACCOUNT_RELEASED',
            message: 'This account has moved to a different guardian. Reconnect it to continue.',
            meta: { retryable: false, released_at: '2026-07-06T10:00:00Z' },
          }),
      });

      const error = await client.getPubkey().catch((e) => e);
      expect(error).toBeInstanceOf(GuardianHttpError);
      expect(error.status).toBe(409);
      expect(error.code).toBe('account_released');
      expect(error.rawCode).toBe('GUARDIAN_ACCOUNT_RELEASED');
      expect(error.releasedAt).toBe('2026-07-06T10:00:00Z');
    });

    it('exposes code without releasedAt for other envelope errors', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: false,
        headers: new Headers(),
        status: 404,
        statusText: 'Not Found',
        text: async () =>
          JSON.stringify({
            code: 'account_not_found',
            message: "We couldn't find that. It may have been completed or removed.",
            meta: { retryable: false },
          }),
      });

      const error = await client.getPubkey().catch((e) => e);
      expect(error.code).toBe('account_not_found');
      expect(error.releasedAt).toBeNull();
    });
  });

  describe('getStatus', () => {
    it('maps the server status response to camelCase', async () => {
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          status: 'ok',
          version: '0.1.0',
          git_commit: 'abc123def456',
          environment: 'devnet',
          started_at: '2026-06-17T10:00:00Z',
          uptime_seconds: 3600,
        }),
      });

      const status = await client.getStatus();

      expect(status).toEqual({
        status: 'ok',
        version: '0.1.0',
        gitCommit: 'abc123def456',
        environment: 'devnet',
        startedAt: '2026-06-17T10:00:00Z',
        uptimeSeconds: 3600,
      });
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/status',
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
        headers: new Headers(),
        status: 503,
        statusText: 'Service Unavailable',
        text: async () => 'down',
      });

      const error = await client.getStatus().catch((e) => e);
      expect(error).toBeInstanceOf(GuardianHttpError);
      expect(error.status).toBe(503);
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

    it('retries a configure replay rejection with fresh authentication', async () => {
      client.setSigner(mockSigner);
      const signRequest = mockSigner.signRequest;
      if (!signRequest) {
        throw new Error('test signer must implement signRequest');
      }
      const signRequestMock = vi.mocked(signRequest);
      signRequestMock.mockImplementation(
        (_accountId, timestamp) => `0x${timestamp.toString(16).padStart(128, '0')}`
      );
      mockFetch
        .mockResolvedValueOnce({
          ok: false,
          headers: new Headers(),
          status: 401,
          statusText: 'Unauthorized',
          text: async () =>
            JSON.stringify({
              code: 'authentication_replay',
              message: 'Guardian received this request out of order. Please try again.',
              meta: { retryable: true },
            }),
        })
        .mockResolvedValueOnce({
          ok: true,
          json: async () => ({ success: true, message: 'Account configured' }),
        });

      await client.configure({
        accountId: '0x' + 'd'.repeat(30),
        auth: {
          MidenFalconRpo: {
            cosigner_commitments: ['0x' + 'e'.repeat(64)],
          },
        },
        initialState: { data: 'base64data', accountId: '0x' + 'd'.repeat(30) },
      });

      expect(mockFetch).toHaveBeenCalledTimes(2);
      const timestamps = mockFetch.mock.calls.map((call) =>
        Number((call[1].headers as Record<string, string>)['x-timestamp'])
      );
      expect(timestamps[1]).toBeGreaterThan(timestamps[0]);
      expect(signRequestMock).toHaveBeenCalledTimes(2);
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

  describe('abandonCandidate', () => {
    it('should record an abandon intent and map the response to camelCase', async () => {
      client.setSigner(mockSigner);

      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({
          account_id: '0x' + 'a'.repeat(30),
          nonce: 7,
          state: 'pending',
          abandon_requested_at: '2026-07-14T12:00:00Z',
        }),
      });

      const result = await client.abandonCandidate('0x' + 'a'.repeat(30), 7);

      expect(result).toEqual({
        accountId: '0x' + 'a'.repeat(30),
        nonce: 7,
        state: 'pending',
        abandonRequestedAt: '2026-07-14T12:00:00Z',
      });

      // Wire format is snake_case
      expect(mockFetch).toHaveBeenCalledWith(
        'http://localhost:3000/delta/candidate/abandon',
        expect.objectContaining({
          method: 'POST',
          body: JSON.stringify({
            account_id: '0x' + 'a'.repeat(30),
            nonce: 7,
          }),
        })
      );
    });

    it('surfaces 409 GUARDIAN_CANDIDATE_LANDED with a parseable error envelope', async () => {
      client.setSigner(mockSigner);

      mockFetch.mockResolvedValueOnce({
        ok: false,
        headers: new Headers(),
        status: 409,
        statusText: 'Conflict',
        text: async () =>
          JSON.stringify({
            code: 'GUARDIAN_CANDIDATE_LANDED',
            message: "This transaction already went through, so it can't be abandoned.",
            meta: { retryable: false },
          }),
      });

      const error = await client
        .abandonCandidate('0x' + 'a'.repeat(30), 7)
        .catch((e) => e);
      expect(error).toBeInstanceOf(GuardianHttpError);
      expect(error.status).toBe(409);
      expect(error.code).toBe('candidate_landed');
      expect(error.rawCode).toBe('GUARDIAN_CANDIDATE_LANDED');
    });

    it('surfaces 404 delta_not_found when no candidate exists at the nonce', async () => {
      client.setSigner(mockSigner);

      mockFetch.mockResolvedValueOnce({
        ok: false,
        headers: new Headers(),
        status: 404,
        statusText: 'Not Found',
        text: async () =>
          JSON.stringify({
            code: 'delta_not_found',
            message: "We couldn't find that. It may have been completed or removed.",
            meta: { retryable: false },
          }),
      });

      const error = await client
        .abandonCandidate('0x' + 'a'.repeat(30), 7)
        .catch((e) => e);
      expect(error).toBeInstanceOf(GuardianHttpError);
      expect(error.status).toBe(404);
      expect(error.code).toBe('delta_not_found');
    });
  });

  describe('abandonStatus', () => {
    const serverDelta = (status: object) => ({
      account_id: '0x' + 'a'.repeat(30),
      nonce: 7,
      prev_commitment: '0x' + 'b'.repeat(64),
      delta_payload: { tx_summary: { data: 'base64summary' }, signatures: [] },
      status,
    });

    it('classifies a still-pending candidate as waiting', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () =>
          serverDelta({ status: 'candidate', timestamp: '2026-07-14T12:00:00Z' }),
      });
      expect(await client.abandonStatus('0x' + 'a'.repeat(30), 7)).toBe('waiting');
    });

    it('classifies a canonicalized delta as landed', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () =>
          serverDelta({ status: 'canonical', timestamp: '2026-07-14T12:00:00Z' }),
      });
      expect(await client.abandonStatus('0x' + 'a'.repeat(30), 7)).toBe('landed');
    });

    it('classifies a retained delta as retained, not abandoned (issue #345)', async () => {
      // The Guardian gave up verifying and released the account, but the
      // on-chain outcome is still uncertain: 'retained' means "unlocked
      // but unresolved" — reporting it as 'abandoned' would wrongly
      // imply the transaction definitively did not land.
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () =>
          serverDelta({
            status: 'retained',
            timestamp: '2026-07-14T12:00:00Z',
            reason: 'retry_exhausted',
          }),
      });
      expect(await client.abandonStatus('0x' + 'a'.repeat(30), 7)).toBe('retained');
    });

    it('classifies a client-abandoned discard as abandoned', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () =>
          serverDelta({
            status: 'discarded',
            timestamp: '2026-07-14T12:00:00Z',
            reason: 'client_abandoned',
          }),
      });
      expect(await client.abandonStatus('0x' + 'a'.repeat(30), 7)).toBe('abandoned');
    });

    it('classifies a reasonless discard and a missing delta as unexpected', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () =>
          serverDelta({ status: 'discarded', timestamp: '2026-07-14T12:00:00Z' }),
      });
      expect(await client.abandonStatus('0x' + 'a'.repeat(30), 7)).toBe('unexpected');

      mockFetch.mockResolvedValueOnce({
        ok: false,
        headers: new Headers(),
        status: 404,
        statusText: 'Not Found',
        text: async () =>
          JSON.stringify({
            code: 'delta_not_found',
            message: "We couldn't find that. It may have been completed or removed.",
            meta: { retryable: false },
          }),
      });
      expect(await client.abandonStatus('0x' + 'a'.repeat(30), 7)).toBe('unexpected');
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

  // --- getDeltaHistory (issue #413) -----

  describe('getDeltaHistory', () => {
    const accountId = '0x' + 'a'.repeat(30);

    const serverPage = {
      items: [
        {
          nonce: 5,
          timestamp: '2026-08-01T12:00:05Z',
          new_commitment: '0x' + 'c'.repeat(64),
          input_notes: [],
          output_notes: [
            {
              note_id: '0x' + 'd'.repeat(64),
              tag: 'p2id',
              assets: [
                { asset_id: '0x' + 'e'.repeat(30), kind: 'fungible', amount: '100' },
              ],
              sender: accountId,
              recipient: '0x' + 'f'.repeat(30),
            },
          ],
        },
        {
          nonce: 4,
          timestamp: '2026-08-01T12:00:04Z',
          new_commitment: null,
          input_notes: [],
          output_notes: [],
          decode_warnings: [{ section: 'tx_summary', reason: 'malformed_tx_summary' }],
        },
      ],
      next_cursor: 'opaque-cursor',
    };

    it('returns a camelCase page and maps notes and warnings', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => serverPage,
      });

      const result = await client.getDeltaHistory(accountId, { limit: 2 });

      expect(result.nextCursor).toBe('opaque-cursor');
      expect(result.entries).toHaveLength(2);
      expect(result.entries[0].nonce).toBe(5);
      expect(result.entries[0].newCommitment).toBe('0x' + 'c'.repeat(64));
      expect(result.entries[0].decodeWarnings).toEqual([]);
      expect(result.entries[0].outputNotes[0]).toEqual({
        noteId: '0x' + 'd'.repeat(64),
        tag: 'p2id',
        assets: [{ assetId: '0x' + 'e'.repeat(30), kind: 'fungible', amount: '100' }],
        sender: accountId,
        recipient: '0x' + 'f'.repeat(30),
      });
      expect(result.entries[1].newCommitment).toBeUndefined();
      expect(result.entries[1].decodeWarnings).toEqual([
        { section: 'tx_summary', reason: 'malformed_tx_summary' },
      ]);

      expect(mockFetch).toHaveBeenCalledWith(
        `http://localhost:3000/delta/history?account_id=${accountId}&limit=2`,
        expect.objectContaining({
          method: 'GET',
          headers: expect.objectContaining({
            'x-pubkey': mockSigner.publicKey,
          }),
        })
      );
    });

    it('signs the same key set it sends: omitted options stay omitted', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ items: [], next_cursor: null }),
      });

      const result = await client.getDeltaHistory(accountId);

      expect(result.entries).toEqual([]);
      expect(result.nextCursor).toBeUndefined();
      expect(mockFetch).toHaveBeenCalledWith(
        `http://localhost:3000/delta/history?account_id=${accountId}`,
        expect.anything()
      );
      // The signed payload must byte-match the server's canonical JSON
      // of its query struct: only account_id when limit/cursor are
      // omitted, and limit as a string when present.
      const signRequest = mockSigner.signRequest as ReturnType<typeof vi.fn>;
      const lastCall = signRequest.mock.calls.at(-1);
      if (lastCall === undefined) {
        throw new Error('Expected signRequest to be called');
      }
      const authPayload = lastCall[2];
      expect(authPayload.toCanonicalJson()).toBe(JSON.stringify({ account_id: accountId }));
    });

    it('passes the cursor through query and signed payload', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValueOnce({
        ok: true,
        json: async () => ({ items: [], next_cursor: null }),
      });

      await client.getDeltaHistory(accountId, { limit: 10, cursor: 'abc123' });

      expect(mockFetch).toHaveBeenCalledWith(
        `http://localhost:3000/delta/history?account_id=${accountId}&limit=10&cursor=abc123`,
        expect.anything()
      );
      const signRequest = mockSigner.signRequest as ReturnType<typeof vi.fn>;
      const lastCall = signRequest.mock.calls.at(-1);
      if (lastCall === undefined) {
        throw new Error('Expected signRequest to be called');
      }
      const authPayload = lastCall[2];
      expect(authPayload.toCanonicalJson()).toBe(
        JSON.stringify({ account_id: accountId, cursor: 'abc123', limit: '10' })
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
        headers: new Headers(),
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

  it('parses a { code, message, meta } body into structured accessors (feature 009)', () => {
    const body = JSON.stringify({
      code: 'rate_limit_exceeded',
      message: 'Too many requests — please try again shortly.',
      meta: { retryable: true, retry_after_secs: 30 },
    });
    const error = new GuardianHttpError(429, 'Too Many Requests', body);
    expect(error.code).toBe('rate_limit_exceeded');
    expect(error.userMessage).toBe('Too many requests — please try again shortly.');
    expect(error.meta?.retryable).toBe(true);
    expect(error.meta?.retryAfterSecs).toBe(30); // snake_case → camelCase
  });

  it('leaves accessors undefined for a non-JSON / non-conforming body', () => {
    const plain = new GuardianHttpError(502, 'Bad Gateway', 'upstream exploded');
    expect(plain.code).toBeNull();
    expect(plain.releasedAt).toBeNull();
    expect(plain.userMessage).toBeUndefined();
    expect(plain.meta).toBeUndefined();
  });

  describe('retry classification (shared fixture)', () => {
    interface FixtureCase {
      name: string;
      body: Record<string, unknown> | null;
      http: { status: number; retryAfterHeader?: string };
      expected: { retryable: boolean; retryAfterSecs: number | null };
    }

    const fixture = JSON.parse(
      readFileSync(
        new URL('../../../fixtures/guardian-client/rate-limit-policy.json', import.meta.url),
        'utf-8'
      )
    ) as { cases: FixtureCase[] };

    it.each(fixture.cases.map((c) => [c.name, c] as const))('%s', (_name, c) => {
      const body = c.body === null ? 'plain failure' : JSON.stringify(c.body);
      const error = new GuardianHttpError(
        c.http.status,
        'error',
        body,
        c.http.retryAfterHeader ?? null
      );

      expect(error.isRetryable()).toBe(c.expected.retryable);
      expect(error.retryAfterSecs()).toBe(c.expected.retryAfterSecs ?? undefined);
    });
  });

  describe('error envelope contract (account-paused path)', () => {
    let client: GuardianHttpClient;
    beforeEach(() => {
      client = new GuardianHttpClient('http://localhost:3000');
      mockFetch.mockReset();
    });

    it('surfaces 409 GUARDIAN_ACCOUNT_PAUSED with a parseable error envelope on pushDeltaProposal', async () => {
      client.setSigner(mockSigner);

      // The server's GuardianError::AccountPaused → IntoResponse contract,
      // reshaped to { code, message, meta } (feature 009). Locks client/server
      // in lockstep: a regression to the legacy { success, error, paused_* } or
      // "(400, {delta: {account_id: 'error text'}})" shape would break this.
      const envelope = {
        code: 'GUARDIAN_ACCOUNT_PAUSED',
        message: "This account is paused and can't approve transactions right now.",
        meta: {
          retryable: false,
          paused_at: '2026-05-20T10:00:00Z',
          paused_reason: 'compliance review',
        },
      };

      mockFetch.mockResolvedValueOnce({
        ok: false,
        headers: new Headers(),
        status: 409,
        statusText: 'Conflict',
        text: async () => JSON.stringify(envelope),
      });

      const request = {
        accountId: '0x' + 'a'.repeat(30),
        nonce: 1,
        deltaPayload: { txSummary: { data: '' }, signatures: [] },
      };

      const error = await client
        .pushDeltaProposal(request)
        .catch((e) => e as GuardianHttpError);

      expect(error).toBeInstanceOf(GuardianHttpError);
      const e = error as GuardianHttpError;
      expect(e.status).toBe(409);

      // Structured accessors parsed from { code, message, meta } (feature 009).
      expect(e.code).toBe('account_paused');
      expect(e.rawCode).toBe('GUARDIAN_ACCOUNT_PAUSED');
      expect(typeof e.userMessage).toBe('string');
      expect(e.userMessage).not.toContain('compliance review'); // sanitized
      expect(e.meta?.retryable).toBe(false);
      expect(e.meta?.pausedAt).toBe('2026-05-20T10:00:00Z');
      expect(e.meta?.pausedReason).toBe('compliance review');

      const parsed = JSON.parse(e.body);
      // Legacy fields gone; not a domain object.
      expect(parsed.success).toBeUndefined();
      expect(parsed.error).toBeUndefined();
      expect(parsed.delta).toBeUndefined();
    });

    it('does not retry a terminal 401 authentication_failed rejection', async () => {
      client.setSigner(mockSigner);
      const envelope = {
        code: 'authentication_failed',
        message: 'Guardian could not authenticate this request.',
        meta: { retryable: false },
      };
      mockFetch.mockResolvedValue({
        ok: false,
        headers: new Headers(),
        status: 401,
        statusText: 'Unauthorized',
        text: async () => JSON.stringify(envelope),
      });

      const error = await client
        .pushDeltaProposal({
          accountId: '0x' + 'a'.repeat(30),
          nonce: 1,
          deltaPayload: { txSummary: { data: '' }, signatures: [] },
        })
        .catch((e) => e as GuardianHttpError);

      expect(error).toBeInstanceOf(GuardianHttpError);
      const e = error as GuardianHttpError;
      expect(e.status).toBe(401);
      expect(e.code).toBe('authentication_failed');
      expect(typeof e.userMessage).toBe('string');
      expect(mockFetch).toHaveBeenCalledTimes(1);
      const parsed = JSON.parse(e.body);
      expect(parsed.success).toBeUndefined();
      expect(parsed.delta).toBeUndefined();
    });

    it('retries an authentication_replay rejection with a fresh timestamp and signature', async () => {
      client.setSigner(mockSigner);
      const signRequest = mockSigner.signRequest;
      if (!signRequest) {
        throw new Error('test signer must implement signRequest');
      }
      const signRequestMock = vi.mocked(signRequest);
      signRequestMock.mockClear();
      const signatureForTimestamp = (_accountId: string, timestamp: number) =>
        `0x${timestamp.toString(16).padStart(128, '0')}`;
      signRequestMock
        .mockImplementationOnce(signatureForTimestamp)
        .mockImplementationOnce(signatureForTimestamp);
      const replayResponse = {
        ok: false,
        headers: new Headers(),
        status: 401,
        statusText: 'Unauthorized',
        text: async () =>
          JSON.stringify({
            code: 'authentication_replay',
            message: 'Guardian received this request out of order. Please try again.',
            meta: { retryable: true },
          }),
      };
      const serverResponse = {
        delta: {
          account_id: '0x' + 'a'.repeat(30),
          nonce: 1,
          prev_commitment: '0x' + 'b'.repeat(64),
          delta_payload: { tx_summary: { data: '' }, signatures: [] },
          status: {
            status: 'pending',
            timestamp: '2024-01-01T00:00:00Z',
            proposer_id: '0x' + 'c'.repeat(64),
            cosigner_sigs: [],
          },
        },
        commitment: '0x' + 'd'.repeat(64),
      };
      mockFetch
        .mockResolvedValueOnce(replayResponse)
        .mockResolvedValueOnce({ ok: true, json: async () => serverResponse });

      const result = await client.pushDeltaProposal({
        accountId: '0x' + 'a'.repeat(30),
        nonce: 1,
        deltaPayload: { txSummary: { data: '' }, signatures: [] },
      });

      expect(result.commitment).toBe('0x' + 'd'.repeat(64));
      expect(mockFetch).toHaveBeenCalledTimes(2);
      const timestamps = mockFetch.mock.calls.map((call) =>
        Number((call[1].headers as Record<string, string>)['x-timestamp'])
      );
      const signatures = mockFetch.mock.calls.map(
        (call) => (call[1].headers as Record<string, string>)['x-signature']
      );
      expect(timestamps[1]).toBeGreaterThan(timestamps[0]);
      expect(signatures[1]).not.toBe(signatures[0]);
      expect(signRequestMock).toHaveBeenCalledTimes(2);
    });

    it('gives up after exhausting the bounded replay retry budget', async () => {
      client.setSigner(mockSigner);
      mockFetch.mockResolvedValue({
        ok: false,
        headers: new Headers(),
        status: 401,
        statusText: 'Unauthorized',
        text: async () =>
          JSON.stringify({
            code: 'authentication_replay',
            message: 'Guardian received this request out of order. Please try again.',
            meta: { retryable: true },
          }),
      });

      const error = await client
        .pushDeltaProposal({
          accountId: '0x' + 'a'.repeat(30),
          nonce: 1,
          deltaPayload: { txSummary: { data: '' }, signatures: [] },
        })
        .catch((e) => e as GuardianHttpError);

      expect(error).toBeInstanceOf(GuardianHttpError);
      const e = error as GuardianHttpError;
      expect(e.code).toBe('authentication_replay');
      expect(e.meta?.retryable).toBe(true);
      expect(mockFetch).toHaveBeenCalledTimes(3);
    });
  });
});
