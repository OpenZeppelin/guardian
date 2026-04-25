import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  GuardianOperatorContractError,
  GuardianOperatorHttpClient,
  GuardianOperatorHttpError,
} from './http.js';

const mockFetch = vi.fn();
vi.stubGlobal('fetch', mockFetch);

describe('GuardianOperatorHttpClient', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', mockFetch);
    mockFetch.mockReset();
  });

  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it('requests an operator challenge and parses the response', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      success: true,
      challenge: {
        domain: '*',
        commitment: '0xabc',
        nonce: 'nonce-1',
        expires_at: '2026-04-22T12:00:00Z',
        signing_digest: '0xdef',
      },
    }));

    const client = new GuardianOperatorHttpClient({
      baseUrl: 'https://guardian.example',
      credentials: 'include',
    });

    const response = await client.challenge('0xabc');

    expect(response).toEqual({
      success: true,
      challenge: {
        domain: '*',
        commitment: '0xabc',
        nonce: 'nonce-1',
        expiresAt: '2026-04-22T12:00:00Z',
        signingDigest: '0xdef',
      },
    });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/auth/challenge?commitment=0xabc',
      expect.objectContaining({
        method: 'GET',
        credentials: 'include',
        headers: expect.any(Headers),
      }),
    );

    const headers = mockFetch.mock.calls[0]?.[1]?.headers as Headers;
    expect(headers.get('Accept')).toBe('application/json');
    expect(headers.get('Content-Type')).toBeNull();
  });

  it('verifies a signed challenge using the provided commitment and signature', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      success: true,
      operator_id: 'operator-1',
      expires_at: '2026-04-22T18:00:00Z',
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.verify({
      commitment: '0xabc',
      signature: '0xsig',
    });

    expect(response).toEqual({
      success: true,
      operatorId: 'operator-1',
      expiresAt: '2026-04-22T18:00:00Z',
    });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/auth/verify',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({
          commitment: '0xabc',
          signature: '0xsig',
        }),
      }),
    );

    const headers = mockFetch.mock.calls[0]?.[1]?.headers as Headers;
    expect(headers.get('Accept')).toBe('application/json');
    expect(headers.get('Content-Type')).toBe('application/json');
  });

  it('lists dashboard accounts and maps snake_case fields to camelCase', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      success: true,
      total_count: 1,
      accounts: [
        {
          account_id: 'acc-1',
          auth_scheme: 'falcon',
          authorized_signer_count: 2,
          has_pending_candidate: false,
          current_commitment: '0x123',
          state_status: 'available',
          created_at: '2026-04-22T10:00:00Z',
          updated_at: '2026-04-22T11:00:00Z',
        },
      ],
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.listAccounts();

    expect(response).toEqual({
      success: true,
      totalCount: 1,
      accounts: [
        {
          accountId: 'acc-1',
          authScheme: 'falcon',
          authorizedSignerCount: 2,
          hasPendingCandidate: false,
          currentCommitment: '0x123',
          stateStatus: 'available',
          createdAt: '2026-04-22T10:00:00Z',
          updatedAt: '2026-04-22T11:00:00Z',
        },
      ],
    });
  });

  it('encodes opaque account ids when fetching one account', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      success: true,
      account: {
        account_id: 'acct/with space',
        auth_scheme: 'falcon',
        authorized_signer_count: 1,
        authorized_signer_ids: ['0xaaa'],
        has_pending_candidate: true,
        current_commitment: null,
        state_status: 'unavailable',
        created_at: '2026-04-22T10:00:00Z',
        updated_at: '2026-04-22T11:00:00Z',
        state_created_at: null,
        state_updated_at: null,
      },
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example/api');
    const response = await client.getAccount('acct/with space');

    expect(response.account.accountId).toBe('acct/with space');
    expect(response.account.authorizedSignerIds).toEqual(['0xaaa']);
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/api/dashboard/accounts/acct%2Fwith%20space',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('logs out with a POST request and parses the response', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      success: true,
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.logout();

    expect(response).toEqual({
      success: true,
    });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/auth/logout',
      expect.objectContaining({ method: 'POST' }),
    );
  });

  it('returns a structured HTTP error when the server responds with JSON error data', async () => {
    mockFetch.mockResolvedValueOnce(errorResponse({
      status: 429,
      statusText: 'Too Many Requests',
      body: {
        success: false,
        error: 'Rate limit exceeded',
        retry_after_secs: 60,
      },
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const error = await client.listAccounts().catch((value) => value);

    expect(error).toBeInstanceOf(GuardianOperatorHttpError);
    expect(error.status).toBe(429);
    expect(error.data).toEqual({
      success: false,
      error: 'Rate limit exceeded',
      retryAfterSecs: 60,
    });
    expect(error.retryAfterSecs).toBe(60);
  });

  it('throws a contract error for malformed success payloads', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      success: true,
      total_count: 'not-a-number',
      accounts: [],
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');

    await expect(client.listAccounts()).rejects.toBeInstanceOf(
      GuardianOperatorContractError,
    );
  });

  it('uses a custom fetch implementation when provided', async () => {
    const customFetch = vi.fn().mockResolvedValue(okJson({
      success: true,
      challenge: {
        domain: '*',
        commitment: '0xabc',
        nonce: 'nonce-1',
        expires_at: '2026-04-22T12:00:00Z',
        signing_digest: '0xdef',
      },
    }));

    const client = new GuardianOperatorHttpClient({
      baseUrl: 'https://guardian.example',
      fetch: customFetch,
    });

    await client.challenge('0xabc');
    expect(customFetch).toHaveBeenCalledTimes(1);
    expect(mockFetch).not.toHaveBeenCalled();
  });

  it('supports relative base URLs in browser environments', async () => {
    vi.stubGlobal('location', { href: 'http://127.0.0.1:3003/' });
    mockFetch.mockResolvedValueOnce(okJson({
      success: true,
      challenge: {
        domain: '*',
        commitment: '0xabc',
        nonce: 'nonce-1',
        expires_at: '2026-04-22T12:00:00Z',
        signing_digest: '0xdef',
      },
    }));

    const client = new GuardianOperatorHttpClient({
      baseUrl: '/guardian',
      credentials: 'include',
    });

    await client.challenge('0xabc');

    expect(mockFetch).toHaveBeenCalledWith(
      'http://127.0.0.1:3003/guardian/auth/challenge?commitment=0xabc',
      expect.objectContaining({
        method: 'GET',
        credentials: 'include',
      }),
    );
  });
});

function okJson(payload: unknown) {
  return {
    ok: true,
    json: async () => payload,
  };
}

function errorResponse(input: {
  status: number;
  statusText: string;
  body: unknown;
}) {
  return {
    ok: false,
    status: input.status,
    statusText: input.statusText,
    text: async () => JSON.stringify(input.body),
  };
}
