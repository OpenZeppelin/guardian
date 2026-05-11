import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  GuardianOperatorContractError,
  GuardianOperatorHttpClient,
  GuardianOperatorHttpError,
  isDashboardErrorCode,
  parseErrorBody,
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

  it('lists dashboard accounts via the paged envelope (breaking change vs 003)', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
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
        next_cursor: null,
      }),
    );

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.listAccounts();

    expect(response).toEqual({
      items: [
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
      nextCursor: null,
    });
  });

  it('passes limit and cursor query params to the account list endpoint', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({ items: [], next_cursor: null }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listAccounts({ limit: 25, cursor: 'token-1' });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts?limit=25&cursor=token-1',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('rejects the account list with InvalidLimit on out-of-range limit', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 400,
        statusText: 'Bad Request',
        body: {
          success: false,
          code: 'invalid_limit',
          error: 'limit must be at most 500, got 9999',
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const err = (await client
      .listAccounts({ limit: 9999 })
      .catch((value) => value)) as GuardianOperatorHttpError;
    expect(err).toBeInstanceOf(GuardianOperatorHttpError);
    expect(err.status).toBe(400);
    expect(err.data?.code).toBe('invalid_limit');
  });

  it('returns the dashboard info snapshot with degraded markers passed through', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        service_status: 'degraded',
        environment: 'mainnet',
        build: {
          version: '0.14.6',
          git_commit: 'abcdef123456',
          profile: 'release',
          started_at: '2026-05-11T10:00:00Z',
        },
        backend: {
          storage: 'postgres',
          supported_ack_schemes: ['ecdsa', 'falcon'],
          canonicalization: {
            check_interval_seconds: 10,
            max_retries: 48,
            submission_grace_period_seconds: 600,
          },
        },
        total_account_count: 1234,
        accounts_by_auth_method: { miden_falcon: 1200, miden_ecdsa: 34 },
        latest_activity: '2026-05-09T14:00:00Z',
        delta_status_counts: {
          candidate: 7,
          canonical: 8902,
          discarded: 21,
        },
        in_flight_proposal_count: 12,
        degraded_aggregates: ['delta_status_counts'],
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const info = await client.getDashboardInfo();

    expect(info).toEqual({
      serviceStatus: 'degraded',
      environment: 'mainnet',
      build: {
        version: '0.14.6',
        gitCommit: 'abcdef123456',
        profile: 'release',
        startedAt: '2026-05-11T10:00:00Z',
      },
      backend: {
        storage: 'postgres',
        supportedAckSchemes: ['ecdsa', 'falcon'],
        canonicalization: {
          checkIntervalSeconds: 10,
          maxRetries: 48,
          submissionGracePeriodSeconds: 600,
        },
      },
      totalAccountCount: 1234,
      accountsByAuthMethod: { miden_falcon: 1200, miden_ecdsa: 34 },
      latestActivity: '2026-05-09T14:00:00Z',
      deltaStatusCounts: {
        candidate: 7,
        canonical: 8902,
        discarded: 21,
      },
      inFlightProposalCount: 12,
      degradedAggregates: ['delta_status_counts'],
    });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/info',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('handles a healthy info response with null latest_activity', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        service_status: 'healthy',
        environment: 'testnet',
        build: {
          version: '0.0.1',
          git_commit: 'unknown',
          profile: 'debug',
          started_at: '2026-05-11T10:00:00Z',
        },
        backend: {
          storage: 'filesystem',
          supported_ack_schemes: ['ecdsa', 'falcon'],
          canonicalization: null,
        },
        total_account_count: 0,
        accounts_by_auth_method: {},
        latest_activity: null,
        delta_status_counts: { candidate: 0, canonical: 0, discarded: 0 },
        in_flight_proposal_count: 0,
        degraded_aggregates: [],
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const info = await client.getDashboardInfo();
    expect(info.latestActivity).toBeNull();
    expect(info.serviceStatus).toBe('healthy');
    expect(info.degradedAggregates).toEqual([]);
    expect(info.backend.storage).toBe('filesystem');
    expect(info.backend.canonicalization).toBeNull();
    expect(info.accountsByAuthMethod).toEqual({});
  });

  it('encodes opaque account ids when fetching one account', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
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
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example/api');
    const response = await client.getAccount('acct/with space');

    expect(response.accountId).toBe('acct/with space');
    expect(response.authorizedSignerIds).toEqual(['0xaaa']);
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

  it('throws a contract error when the paged envelope is malformed', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        // Missing `items` and `next_cursor`; envelope is invalid.
        accounts: [],
      }),
    );

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

describe('GuardianOperatorHttpClient — per-account history', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', mockFetch);
    mockFetch.mockReset();
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it('lists per-account deltas with default pagination params', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 47,
            status: 'candidate',
            status_timestamp: '2026-05-08T14:22:03Z',
            prev_commitment: '0x7e8f',
            new_commitment: '0xa3b4',
            retry_count: 2,
          },
          {
            nonce: 46,
            status: 'canonical',
            status_timestamp: '2026-05-08T13:15:20Z',
            prev_commitment: '0x6d7e',
            new_commitment: '0x7e8f',
          },
          {
            nonce: 45,
            status: 'discarded',
            status_timestamp: '2026-05-08T12:01:55Z',
            prev_commitment: '0x6d7e',
            new_commitment: null,
          },
        ],
        next_cursor: 'AaBbCc',
      }),
    );

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountDeltas('0xacc');

    expect(page.nextCursor).toBe('AaBbCc');
    expect(page.items).toHaveLength(3);
    expect(page.items[0]).toEqual({
      nonce: 47,
      status: 'candidate',
      statusTimestamp: '2026-05-08T14:22:03Z',
      prevCommitment: '0x7e8f',
      newCommitment: '0xa3b4',
      retryCount: 2,
    });
    expect(page.items[1].retryCount).toBeUndefined();
    expect(page.items[2].newCommitment).toBeNull();

    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xacc/deltas',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('passes limit and cursor query params for delta listing', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({ items: [], next_cursor: null }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listAccountDeltas('0xacc', { limit: 25, cursor: 'token' });

    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xacc/deltas?limit=25&cursor=token',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('returns empty page with null cursor at end of delta history', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({ items: [], next_cursor: null }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountDeltas('0xacc');
    expect(page.items).toEqual([]);
    expect(page.nextCursor).toBeNull();
  });

  it('throws GuardianOperatorHttpError with InvalidLimit code on out-of-range limit', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 400,
        statusText: 'Bad Request',
        body: {
          success: false,
          code: 'invalid_limit',
          error: 'limit must be at most 500, got 9999',
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    try {
      await client.listAccountDeltas('0xacc', { limit: 9999 });
      throw new Error('expected http error');
    } catch (error) {
      expect(error).toBeInstanceOf(GuardianOperatorHttpError);
      const httpErr = error as GuardianOperatorHttpError;
      expect(httpErr.status).toBe(400);
      expect(httpErr.data?.code).toBe('invalid_limit');
      expect(isDashboardErrorCode(httpErr.data?.code ?? '')).toBe(true);
    }
  });

  it('throws GuardianOperatorHttpError with AccountNotFound code on unknown account', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 404,
        statusText: 'Not Found',
        body: {
          success: false,
          code: 'account_not_found',
          error: "Account '0xunknown' not found",
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    try {
      await client.listAccountDeltas('0xunknown');
      throw new Error('expected http error');
    } catch (error) {
      expect(error).toBeInstanceOf(GuardianOperatorHttpError);
      expect((error as GuardianOperatorHttpError).data?.code).toBe(
        'account_not_found',
      );
    }
  });

  it('throws GuardianOperatorHttpError with DataUnavailable code on storage failure', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 503,
        statusText: 'Service Unavailable',
        body: {
          success: false,
          code: 'data_unavailable',
          error: 'delta store unreadable',
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    try {
      await client.listAccountDeltas('0xacc');
      throw new Error('expected http error');
    } catch (error) {
      const httpErr = error as GuardianOperatorHttpError;
      expect(httpErr.status).toBe(503);
      expect(httpErr.data?.code).toBe('data_unavailable');
    }
  });

  it('lists per-account in-flight proposals with all required fields', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            commitment: '0xab12cd34',
            nonce: 48,
            proposer_id: '0xfeed',
            originating_timestamp: '2026-05-08T14:18:50Z',
            signatures_collected: 2,
            signatures_required: 3,
            prev_commitment: '0xa3b4',
            new_commitment: '0xb4c5',
          },
        ],
        next_cursor: null,
      }),
    );

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountProposals('0xacc');

    expect(page.items[0]).toEqual({
      commitment: '0xab12cd34',
      nonce: 48,
      proposerId: '0xfeed',
      originatingTimestamp: '2026-05-08T14:18:50Z',
      signaturesCollected: 2,
      signaturesRequired: 3,
      prevCommitment: '0xa3b4',
      newCommitment: '0xb4c5',
    });
  });

  it('returns empty proposal page for single-key Miden / EVM accounts', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({ items: [], next_cursor: null }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountProposals('0xacc');
    expect(page.items).toEqual([]);
    expect(page.nextCursor).toBeNull();
  });

  it('encodes account_id segment for path-routed history endpoints', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({ items: [], next_cursor: null }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    // A canonical Miden ID can contain colons; ensure they are URL-encoded.
    await client.listAccountDeltas('miden:0xabc:def');
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/miden%3A0xabc%3Adef/deltas',
      expect.objectContaining({ method: 'GET' }),
    );
  });
});

describe('parseErrorBody', () => {
  it('extracts and narrows known dashboard error codes from a Response', async () => {
    const cases: Array<{ code: string; message: string }> = [
      { code: 'invalid_cursor', message: 'cursor signature mismatch' },
      { code: 'invalid_limit', message: 'limit must be in [1, 500]' },
      {
        code: 'invalid_status_filter',
        message: "unknown status value 'foo'",
      },
      { code: 'data_unavailable', message: 'delta store unreadable' },
      { code: 'account_not_found', message: "Account 'x' not found" },
    ];

    for (const { code, message } of cases) {
      const response = errorResponse({
        status: 400,
        statusText: 'Bad Request',
        body: { success: false, code, error: message },
      });
      const parsed = await parseErrorBody(response as unknown as Response);
      expect(parsed.code).toBe(code);
      expect(parsed.message).toBe(message);
      expect(isDashboardErrorCode(code)).toBe(true);
    }
  });

  it('returns the raw code string for codes outside the dashboard taxonomy', async () => {
    // Pick a non-dashboard code that the broader Guardian server can
    // emit (e.g. rate-limit) — `parseErrorBody` returns it verbatim
    // as a string, but `isDashboardErrorCode` narrows to `false`.
    const response = errorResponse({
      status: 429,
      statusText: 'Too Many Requests',
      body: { success: false, code: 'rate_limit_exceeded', error: 'slow down' },
    });
    const parsed = await parseErrorBody(response as unknown as Response);
    expect(parsed.code).toBe('rate_limit_exceeded');
    expect(isDashboardErrorCode('rate_limit_exceeded')).toBe(false);
  });

  it('forwards retry_after_secs when present', async () => {
    const response = errorResponse({
      status: 429,
      statusText: 'Too Many Requests',
      body: {
        success: false,
        code: 'rate_limit_exceeded',
        error: 'slow down',
        retry_after_secs: 7,
      },
    });
    const parsed = await parseErrorBody(response as unknown as Response);
    expect(parsed.retryAfterSecs).toBe(7);
  });

  it('returns null code when the body is missing', async () => {
    const response = {
      ok: false,
      status: 500,
      statusText: 'Internal',
      text: async () => '',
    };
    const parsed = await parseErrorBody(response as unknown as Response);
    expect(parsed.code).toBeNull();
    expect(parsed.message).toBeNull();
  });

  it('returns null code when the body is malformed JSON', async () => {
    const response = {
      ok: false,
      status: 500,
      statusText: 'Internal',
      text: async () => 'not json',
    };
    const parsed = await parseErrorBody(response as unknown as Response);
    expect(parsed.code).toBeNull();
    expect(parsed.message).toBeNull();
  });

  it('accepts a pre-parsed object body', async () => {
    const parsed = await parseErrorBody({
      success: false,
      code: 'invalid_cursor',
      error: 'tampered',
    });
    expect(parsed.code).toBe('invalid_cursor');
    expect(parsed.message).toBe('tampered');
  });
});

describe('GuardianOperatorHttpClient — global feeds (US6, US7)', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', mockFetch);
    mockFetch.mockReset();
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it('lists global deltas with required account_id on every entry', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 9023,
            account_id: '0xacc1',
            status: 'candidate',
            status_timestamp: '2026-05-09T14:22:03Z',
            prev_commitment: '0x7e8f',
            new_commitment: '0xa3b4',
            retry_count: 2,
          },
          {
            nonce: 9022,
            account_id: '0xacc2',
            status: 'canonical',
            status_timestamp: '2026-05-09T14:21:48Z',
            prev_commitment: '0x6d7e',
            new_commitment: '0x7e8f',
          },
        ],
        next_cursor: 'next-token',
      }),
    );

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listGlobalDeltas();
    expect(page.items).toHaveLength(2);
    expect(page.items[0]).toEqual({
      nonce: 9023,
      accountId: '0xacc1',
      status: 'candidate',
      statusTimestamp: '2026-05-09T14:22:03Z',
      prevCommitment: '0x7e8f',
      newCommitment: '0xa3b4',
      retryCount: 2,
    });
    expect(page.nextCursor).toBe('next-token');
  });

  it('serializes a single status filter value', async () => {
    mockFetch.mockResolvedValueOnce(okJson({ items: [], next_cursor: null }));
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listGlobalDeltas({ status: 'candidate' });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/deltas?status=candidate',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('serializes an array of status filter values to comma-separated', async () => {
    mockFetch.mockResolvedValueOnce(okJson({ items: [], next_cursor: null }));
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listGlobalDeltas({
      status: ['candidate', 'canonical'],
    });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/deltas?status=candidate%2Ccanonical',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('rejects unknown status filter values via the server response', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 400,
        statusText: 'Bad Request',
        body: {
          success: false,
          code: 'invalid_status_filter',
          error: "unknown status value 'foo'",
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const err = (await client
      .listGlobalDeltas({ status: 'foo' as never })
      .catch((v) => v)) as GuardianOperatorHttpError;
    expect(err.status).toBe(400);
    expect(err.data?.code).toBe('invalid_status_filter');
  });

  it('throws contract error if the server omits account_id on a global delta entry', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 1,
            // account_id intentionally missing
            status: 'canonical',
            status_timestamp: '2026-05-09T10:00:00Z',
            prev_commitment: '0x',
            new_commitment: null,
          },
        ],
        next_cursor: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await expect(client.listGlobalDeltas()).rejects.toBeInstanceOf(
      GuardianOperatorContractError,
    );
  });

  it('lists global proposals with account_id on every entry', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            commitment: '0xab12',
            nonce: 48,
            account_id: '0xacc1',
            proposer_id: '0xfeed',
            originating_timestamp: '2026-05-09T14:18:50Z',
            signatures_collected: 2,
            signatures_required: 3,
            prev_commitment: '0xa3b4',
            new_commitment: '0xb4c5',
          },
        ],
        next_cursor: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listGlobalProposals();
    expect(page.items[0].accountId).toBe('0xacc1');
    expect(page.items[0].signaturesCollected).toBe(2);
  });
});

describe('GuardianOperatorHttpClient — error matrix (FR-028 / SC-012)', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', mockFetch);
    mockFetch.mockReset();
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  // SC-012: every error code in the FR-028 taxonomy is reachable
  // through one of the new dashboard wrappers, and the wrapper
  // surfaces the stable `code` field on the thrown error so
  // consumers can branch on it.
  const matrix: Array<{
    name: string;
    status: number;
    code: string;
    invoke: (client: GuardianOperatorHttpClient) => Promise<unknown>;
  }> = [
    {
      name: '401 Unauthorized on listAccounts',
      status: 401,
      code: 'authentication_failed',
      invoke: (c) => c.listAccounts(),
    },
    {
      name: '404 AccountNotFound on listAccountDeltas',
      status: 404,
      code: 'account_not_found',
      invoke: (c) => c.listAccountDeltas('0xunknown'),
    },
    {
      name: '400 InvalidLimit on listAccounts',
      status: 400,
      code: 'invalid_limit',
      invoke: (c) => c.listAccounts({ limit: 9999 }),
    },
    {
      name: '400 InvalidCursor on listAccountProposals',
      status: 400,
      code: 'invalid_cursor',
      invoke: (c) => c.listAccountProposals('0xacc', { cursor: 'tampered' }),
    },
    {
      name: '503 DataUnavailable on listAccountDeltas',
      status: 503,
      code: 'data_unavailable',
      invoke: (c) => c.listAccountDeltas('0xacc'),
    },
    {
      name: '503 DataUnavailable on getDashboardInfo',
      status: 503,
      code: 'data_unavailable',
      invoke: (c) => c.getDashboardInfo(),
    },
  ];

  for (const { name, status, code, invoke } of matrix) {
    it(name, async () => {
      mockFetch.mockResolvedValueOnce(
        errorResponse({
          status,
          statusText: code,
          body: { success: false, code, error: `${code} message` },
        }),
      );
      const client = new GuardianOperatorHttpClient('https://guardian.example');
      const err = (await invoke(client).catch((v) => v)) as GuardianOperatorHttpError;
      expect(err).toBeInstanceOf(GuardianOperatorHttpError);
      expect(err.status).toBe(status);
      expect(err.data?.code).toBe(code);
    });
  }
});

describe('isDashboardErrorCode', () => {
  it('narrows the five-code dashboard taxonomy', () => {
    expect(isDashboardErrorCode('invalid_cursor')).toBe(true);
    expect(isDashboardErrorCode('invalid_limit')).toBe(true);
    expect(isDashboardErrorCode('invalid_status_filter')).toBe(true);
    expect(isDashboardErrorCode('data_unavailable')).toBe(true);
    expect(isDashboardErrorCode('account_not_found')).toBe(true);
    expect(isDashboardErrorCode('authentication_failed')).toBe(true);
  });

  it('rejects unknown codes', () => {
    expect(isDashboardErrorCode('some_other_code')).toBe(false);
    // The server emits `authentication_failed` for 401, not the
    // generic `unauthorized` code that this guard previously
    // accepted by mistake.
    expect(isDashboardErrorCode('unauthorized')).toBe(false);
    expect(isDashboardErrorCode('')).toBe(false);
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
