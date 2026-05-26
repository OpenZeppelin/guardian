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
            paused_at: null,
            paused_reason: null,
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
          pausedAt: null,
          pausedReason: null,
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
      paused_at: null,
      paused_reason: null,
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

  it('parses the account snapshot vault into camelCase', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      commitment: '0xc0ffee',
      updated_at: '2026-05-11T10:00:00Z',
      has_pending_candidate: true,
      vault: {
        fungible: [
          { faucet_id: '0xfa1', amount: '1000000' },
          { faucet_id: '0xfa2', amount: '42' },
        ],
        non_fungible: [
          { faucet_id: '0xnf1', vault_key: '0xdead' },
        ],
      },
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const snapshot = await client.getAccountSnapshot('0xacc');

    expect(snapshot.commitment).toBe('0xc0ffee');
    expect(snapshot.updatedAt).toBe('2026-05-11T10:00:00Z');
    expect(snapshot.hasPendingCandidate).toBe(true);
    expect(snapshot.vault.fungible).toEqual([
      { faucetId: '0xfa1', amount: '1000000' },
      { faucetId: '0xfa2', amount: '42' },
    ]);
    expect(snapshot.vault.nonFungible).toEqual([
      { faucetId: '0xnf1', vaultKey: '0xdead' },
    ]);
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xacc/snapshot',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('encodes opaque account ids when fetching a snapshot', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      commitment: '0xc',
      updated_at: '2026-05-11T10:00:00Z',
      has_pending_candidate: false,
      vault: { fungible: [], non_fungible: [] },
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example/api');
    await client.getAccountSnapshot('acct/with space');

    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/api/dashboard/accounts/acct%2Fwith%20space/snapshot',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('rejects a snapshot fungible entry missing required fields', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      commitment: '0xc',
      updated_at: '2026-05-11T10:00:00Z',
      has_pending_candidate: false,
      vault: {
        // First entry is well-formed; second is missing `amount` —
        // the parser is strict (requireString) so it must throw.
        fungible: [
          { faucet_id: '0xfa1', amount: '1' },
          { faucet_id: '0xfa2' },
        ],
        non_fungible: [],
      },
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await expect(client.getAccountSnapshot('0xacc')).rejects.toBeInstanceOf(
      GuardianOperatorContractError,
    );
  });

  it('rejects a snapshot non-fungible entry missing required fields', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      commitment: '0xc',
      updated_at: '2026-05-11T10:00:00Z',
      has_pending_candidate: false,
      vault: {
        fungible: [],
        non_fungible: [
          { faucet_id: '0xnf1' }, // missing vault_key
        ],
      },
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await expect(client.getAccountSnapshot('0xacc')).rejects.toBeInstanceOf(
      GuardianOperatorContractError,
    );
  });

  // ---------------------------------------------------------------
  // Feature 006-operator-authz US6 / FR-033..FR-036:
  // GET /dashboard/session — operator identity + effective permissions.
  // ---------------------------------------------------------------

  it('fetches /dashboard/session and parses operator_id + permissions', async () => {
    mockFetch.mockResolvedValueOnce(okJson({
      operator_id: '0xabc123',
      permissions: ['accounts:pause', 'dashboard:read'],
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const session = await client.getSession();

    expect(session).toEqual({
      operatorId: '0xabc123',
      permissions: ['accounts:pause', 'dashboard:read'],
    });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/session',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('parses /dashboard/session with an empty permissions array (explicit-empty operator)', async () => {
    // FR-034: an operator with `permissions: []` receives 200 with
    // an empty array, NOT 403. The client must surface this state
    // distinctly from a 401 so the dashboard can render "no
    // capabilities" vs "not logged in".
    mockFetch.mockResolvedValueOnce(okJson({
      operator_id: '0xdef456',
      permissions: [],
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const session = await client.getSession();

    expect(session).toEqual({
      operatorId: '0xdef456',
      permissions: [],
    });
  });

  it('rejects /dashboard/session with an unknown permission string', async () => {
    // Surfacing server/client vocabulary drift as a contract failure
    // is the load-bearing property — a stale client breaking against
    // a new-server permission is better than silently flowing through.
    mockFetch.mockResolvedValueOnce(okJson({
      operator_id: '0xabc123',
      permissions: ['dashboard:read', 'accounts:freeze'],
    }));

    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const error = await client.getSession().catch((value) => value);

    expect(error).toBeInstanceOf(GuardianOperatorContractError);
    expect(String(error)).toContain('accounts:freeze');
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
            category: 'account_storage_change',
            proposal_type: 'add_signer',
          },
          {
            nonce: 46,
            status: 'canonical',
            status_timestamp: '2026-05-08T13:15:20Z',
            prev_commitment: '0x6d7e',
            new_commitment: '0x7e8f',
            category: 'custom',
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
      category: 'account_storage_change',
      proposalType: 'add_signer',
    });
    expect(page.items[0].noteCounts).toBeUndefined();
    expect(page.items[1].retryCount).toBeUndefined();
    expect(page.items[1].category).toBe('custom');
    expect(page.items[1].proposalType).toBeUndefined();
    expect(page.items[1].noteCounts).toBeUndefined();
    expect(page.items[2].newCommitment).toBeNull();
    expect(page.items[2].category).toBeUndefined();

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

  it('parses an enriched p2id multisig entry with L1 spread fields', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 100,
            status: 'canonical',
            status_timestamp: '2026-05-25T08:00:00Z',
            prev_commitment: '0xprev',
            new_commitment: '0xnew',
            category: 'asset_transfer',
            proposal_type: 'p2id',
            assets: [{ asset_id: '0xfaucet', kind: 'fungible', amount: '-100' }],
            counterparty: { account_id: '0xrecipient', direction: 'out' },
            note_counts: { input: 0, output: 1 },
          },
        ],
        next_cursor: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountDeltas('0xacc');
    const entry = page.items[0];
    expect(entry.category).toBe('asset_transfer');
    expect(entry.proposalType).toBe('p2id');
    expect(entry.assets).toEqual([
      {
        assetId: '0xfaucet',
        kind: 'fungible',
        amount: '-100',
      },
    ]);
    expect(entry.counterparty).toEqual({
      accountId: '0xrecipient',
      direction: 'out',
    });
    expect(entry.noteCounts).toEqual({ input: 0, output: 1 });
  });

  it('parses a consume_notes listing with assets and counterparty at L1', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 0,
            status: 'canonical',
            status_timestamp: '2026-05-25T09:16:34Z',
            prev_commitment: '0xprev',
            new_commitment: '0xnew',
            category: 'note_consumption',
            proposal_type: 'consume_notes',
            assets: [
              {
                asset_id: '0x16f6c85d5652c9200879145bfdda93',
                kind: 'fungible',
                amount: '+100000000',
              },
            ],
            counterparty: {
              account_id: '0x7bfb0f38b0fafa103f86a805594170',
              direction: 'in',
            },
            note_counts: { input: 1, output: 0 },
          },
        ],
        next_cursor: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountDeltas('0xacc');
    expect(page.items[0].assets?.[0].amount).toBe('+100000000');
    expect(page.items[0].counterparty?.direction).toBe('in');
  });

  it('parses a single-key push entry with no proposal type', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 200,
            status: 'canonical',
            status_timestamp: '2026-05-25T08:01:00Z',
            prev_commitment: '0xprev',
            new_commitment: '0xnew',
            category: 'account_storage_change',
            note_counts: { input: 0, output: 0 },
          },
        ],
        next_cursor: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountDeltas('0xacc');
    const entry = page.items[0];
    expect(entry.category).toBe('account_storage_change');
    expect(entry.proposalType).toBeUndefined();
    expect(entry.assets).toBeUndefined();
    expect(entry.counterparty).toBeUndefined();
  });

  it('tolerates an entry with no enrichment fields (pre-feature-007 row)', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 250,
            status: 'canonical',
            status_timestamp: '2026-05-25T08:01:30Z',
            prev_commitment: '0xprev',
            new_commitment: '0xnew',
          },
        ],
        next_cursor: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const page = await client.listAccountDeltas('0xacc');
    expect(page.items[0].category).toBeUndefined();
  });

  it('rejects an unknown L1 category value', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: 400,
            status: 'canonical',
            status_timestamp: '2026-05-25T08:03:00Z',
            prev_commitment: '0xprev',
            new_commitment: '0xnew',
            category: 'unicorn_mode',
            note_counts: { input: 0, output: 0 },
          },
        ],
        next_cursor: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await expect(client.listAccountDeltas('0xacc')).rejects.toThrow(
      /invalid category "unicorn_mode"/,
    );
  });

  it('fetches a delta detail with full structured projection', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xacc',
        nonce: 42,
        status: 'canonical',
        status_timestamp: '2026-05-25T09:00:00Z',
        prev_commitment: '0xaaaa',
        new_commitment: '0xbbbb',
        category: 'asset_transfer',
        proposal: { proposal_type: 'p2id' },
        input_notes: [],
        output_notes: [
          {
            note_id: '0xnote1',
            tag: 'custom',
            assets: [
              { asset_id: '0xfaucet', kind: 'fungible', amount: '100' },
            ],
            recipient: '0xrecipient',
          },
        ],
        vault_changes: [
          { kind: 'fungible', asset_id: '0xfaucet', change: '-100' },
        ],
        storage_changes: [],
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const detail = await client.getAccountDeltaDetail('0xacc', 42);
    expect(detail.accountId).toBe('0xacc');
    expect(detail.nonce).toBe(42);
    expect(detail.category).toBe('asset_transfer');
    expect(detail.proposal?.proposalType).toBe('p2id');
    expect(detail.outputNotes).toHaveLength(1);
    expect(detail.outputNotes[0].assets[0]).toEqual({
      assetId: '0xfaucet',
      kind: 'fungible',
      amount: '100',
    });
    expect(detail.outputNotes[0].recipient).toBe('0xrecipient');
    expect(detail.vaultChanges).toEqual([
      { kind: 'fungible', assetId: '0xfaucet', change: '-100' },
    ]);
    expect(detail.storageChanges).toEqual([]);
    expect(detail.decodeWarnings).toBeUndefined();
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xacc/deltas/42',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('serializes a large safe-integer nonce in the URL path', async () => {
    const safeNonce = Number.MAX_SAFE_INTEGER;
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xacc',
        nonce: safeNonce,
        status: 'canonical',
        status_timestamp: '2026-05-25T09:00:00Z',
        prev_commitment: '0xprev',
        new_commitment: '0xnew',
        input_notes: [],
        output_notes: [],
        vault_changes: [],
        storage_changes: [],
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.getAccountDeltaDetail('0xacc', safeNonce);
    expect(mockFetch).toHaveBeenCalledWith(
      `https://guardian.example/dashboard/accounts/0xacc/deltas/${safeNonce}`,
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('requests ?include=raw when includeRaw is set', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xacc',
        nonce: 99,
        status: 'canonical',
        status_timestamp: '2026-05-25T10:00:00Z',
        prev_commitment: '0xprev',
        new_commitment: '0xnew',
        input_notes: [],
        output_notes: [],
        vault_changes: [],
        storage_changes: [],
        raw_transaction_summary: 'dGVzdA==',
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const detail = await client.getAccountDeltaDetail('0xacc', 99, {
      includeRaw: true,
    });
    expect(detail.rawTransactionSummary).toBe('dGVzdA==');
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xacc/deltas/99?include=raw',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('surfaces decode_warnings when sections could not be projected', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xacc',
        nonce: 7,
        status: 'canonical',
        status_timestamp: '2026-05-25T09:01:00Z',
        prev_commitment: '0xp',
        new_commitment: '0xn',
        input_notes: [],
        output_notes: [],
        vault_changes: [],
        storage_changes: [],
        decode_warnings: [
          { section: 'tx_summary', reason: 'malformed_base64' },
        ],
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const detail = await client.getAccountDeltaDetail('0xacc', 7);
    expect(detail.decodeWarnings).toEqual([
      { section: 'tx_summary', reason: 'malformed_base64' },
    ]);
  });

  it('rejects an unknown note tag', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xacc',
        nonce: 8,
        status: 'canonical',
        status_timestamp: '2026-05-25T09:02:00Z',
        prev_commitment: '0xp',
        new_commitment: '0xn',
        input_notes: [],
        output_notes: [
          {
            note_id: '0xnote',
            tag: 'unicorn',
            assets: [],
          },
        ],
        vault_changes: [],
        storage_changes: [],
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await expect(client.getAccountDeltaDetail('0xacc', 8)).rejects.toThrow(
      /unknown note tag "unicorn"/,
    );
  });

  it('parses storage_changes without a before field (v1 after-only)', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xacc',
        nonce: 10,
        status: 'canonical',
        status_timestamp: '2026-05-25T09:03:00Z',
        prev_commitment: '0xp',
        new_commitment: '0xn',
        category: 'account_storage_change',
        input_notes: [],
        output_notes: [],
        vault_changes: [],
        storage_changes: [
          {
            slot_name: 'openzeppelin::multisig::threshold_config',
            after: '0x0200',
          },
        ],
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const detail = await client.getAccountDeltaDetail('0xacc', 10);
    expect(detail.storageChanges).toEqual([
      {
        slotName: 'openzeppelin::multisig::threshold_config',
        after: '0x0200',
      },
    ]);
    expect(detail.storageChanges[0].before).toBeUndefined();
  });

  // The nonce returned by listing must resolve the same delta when
  // passed back to `getAccountDeltaDetail`.
  it('round-trips a listing entry nonce through getAccountDeltaDetail', async () => {
    const accountId = '0xacc';
    const seededNonce = 42;
    mockFetch.mockResolvedValueOnce(
      okJson({
        items: [
          {
            nonce: seededNonce,
            status: 'canonical',
            status_timestamp: '2026-05-25T08:00:00Z',
            prev_commitment: '0xprev',
            new_commitment: '0xnew',
            category: 'asset_transfer',
            proposal_type: 'p2id',
          },
        ],
        next_cursor: null,
      }),
    );
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: accountId,
        nonce: seededNonce,
        status: 'canonical',
        status_timestamp: '2026-05-25T08:00:00Z',
        prev_commitment: '0xprev',
        new_commitment: '0xnew',
        category: 'asset_transfer',
        proposal: { proposal_type: 'p2id' },
        input_notes: [],
        output_notes: [],
        vault_changes: [],
        storage_changes: [],
      }),
    );

    const client = new GuardianOperatorHttpClient('https://guardian.example');

    const page = await client.listAccountDeltas(accountId);
    const listedNonce = page.items[0].nonce;
    expect(listedNonce).toBe(seededNonce);

    const detail = await client.getAccountDeltaDetail(accountId, listedNonce);
    expect(detail.nonce).toBe(listedNonce);
    expect(detail.accountId).toBe(accountId);

    expect(mockFetch).toHaveBeenLastCalledWith(
      `https://guardian.example/dashboard/accounts/${accountId}/deltas/${listedNonce}`,
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
            category: 'custom',
          },
          {
            nonce: 9022,
            account_id: '0xacc2',
            status: 'canonical',
            status_timestamp: '2026-05-09T14:21:48Z',
            prev_commitment: '0x6d7e',
            new_commitment: '0x7e8f',
            category: 'custom',
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
      category: 'custom',
    });
    expect(page.items[0].noteCounts).toBeUndefined();
    expect(page.items[1].noteCounts).toBeUndefined();
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

  it('narrows the feature-006-operator-authz permission-denial code', () => {
    // The typed union uses snake_case; the wire emits SCREAMING_SNAKE
    // and `parseErrorBody` maps at the boundary.
    expect(isDashboardErrorCode('insufficient_operator_permission')).toBe(true);
    expect(
      isDashboardErrorCode('GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION'),
    ).toBe(false);
  });
});

// -----------------------------------------------------------------
// Feature 006-operator-authz, User Story 5: typed permission-denial
// error surface.
// -----------------------------------------------------------------

describe('parseErrorBody (feature 006-operator-authz)', () => {
  it('extracts missing_permissions and retryable on the permission-denial code', async () => {
    const response = errorResponse({
      status: 403,
      statusText: 'Forbidden',
      body: {
        success: false,
        code: 'GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION',
        error: 'Operator lacks required permissions: accounts:pause',
        missing_permissions: ['accounts:pause'],
        retryable: false,
      },
    });
    const parsed = await parseErrorBody(response as unknown as Response);
    // Wire form maps to the typed snake_case surface.
    expect(parsed.code).toBe('insufficient_operator_permission');
    expect(parsed.missingPermissions).toEqual(['accounts:pause']);
    expect(parsed.retryable).toBe(false);
  });

  it('leaves missingPermissions and retryable undefined on every other code', async () => {
    const response = errorResponse({
      status: 404,
      statusText: 'Not Found',
      body: {
        success: false,
        code: 'account_not_found',
        error: "Account 'x' not found",
      },
    });
    const parsed = await parseErrorBody(response as unknown as Response);
    expect(parsed.code).toBe('account_not_found');
    expect(parsed.missingPermissions).toBeUndefined();
    expect(parsed.retryable).toBeUndefined();
  });

  it('preserves lexicographic ordering of missing_permissions from the server', async () => {
    // The server pins lex-sort (FR-017); the client must not
    // re-sort, dedupe, or reorder.
    const response = errorResponse({
      status: 403,
      statusText: 'Forbidden',
      body: {
        success: false,
        code: 'GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION',
        error: 'multiple missing',
        missing_permissions: ['accounts:pause', 'policies:write'],
        retryable: false,
      },
    });
    const parsed = await parseErrorBody(response as unknown as Response);
    expect(parsed.missingPermissions).toEqual([
      'accounts:pause',
      'policies:write',
    ]);
  });

  it('ignores missing_permissions if the code is not the permission-denial one', async () => {
    // Defensive: a buggy server that emits the new field alongside
    // another code should NOT cause clients to surface it as a
    // permission-denial. We strictly gate the field on the code.
    const response = errorResponse({
      status: 404,
      statusText: 'Not Found',
      body: {
        success: false,
        code: 'account_not_found',
        error: 'unrelated',
        missing_permissions: ['accounts:pause'],
        retryable: false,
      },
    });
    const parsed = await parseErrorBody(response as unknown as Response);
    expect(parsed.missingPermissions).toBeUndefined();
    expect(parsed.retryable).toBeUndefined();
  });
});

describe('GuardianOperatorHttpClient — account pausing (feature 001)', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', mockFetch);
    mockFetch.mockReset();
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it('serializes paused=true into the account list URL', async () => {
    mockFetch.mockResolvedValueOnce(okJson({ items: [], next_cursor: null }));
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listAccounts({ paused: true });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts?paused=true',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('serializes paused=false into the account list URL', async () => {
    mockFetch.mockResolvedValueOnce(okJson({ items: [], next_cursor: null }));
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listAccounts({ paused: false });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts?paused=false',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('parses a successful pauseAccount response', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: 'acc-1',
        before_state: 'active',
        after_state: 'paused',
        paused_at: '2026-05-20T10:00:00Z',
        paused_reason: 'compliance review',
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.pauseAccount('acc-1', 'compliance review');
    expect(response).toEqual({
      accountId: 'acc-1',
      beforeState: 'active',
      afterState: 'paused',
      pausedAt: '2026-05-20T10:00:00Z',
      pausedReason: 'compliance review',
    });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/acc-1/pause',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ reason: 'compliance review' }),
      }),
    );
  });

  it('parses a successful unpauseAccount response with a reason', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: 'acc-1',
        before_state: 'paused',
        after_state: 'active',
        reason: 'cleared',
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.unpauseAccount('acc-1', 'cleared');
    expect(response).toEqual({
      accountId: 'acc-1',
      beforeState: 'paused',
      afterState: 'active',
      reason: 'cleared',
    });
  });

  it('parses an unpauseAccount response with null reason', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: 'acc-1',
        before_state: 'paused',
        after_state: 'active',
        reason: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.unpauseAccount('acc-1');
    expect(response.reason).toBeNull();
  });

  it('normalizes a 409 GUARDIAN_ACCOUNT_PAUSED response to account_paused with pausedAt/pausedReason', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 409,
        statusText: 'Conflict',
        body: {
          success: false,
          code: 'GUARDIAN_ACCOUNT_PAUSED',
          error: 'account is paused',
          paused_at: '2026-05-20T10:00:00Z',
          paused_reason: 'compliance review',
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await expect(
      client.pauseAccount('acc-1', 'retry'),
    ).rejects.toMatchObject({
      data: {
        code: 'account_paused',
        pausedAt: '2026-05-20T10:00:00Z',
        pausedReason: 'compliance review',
      },
    });
  });
});

describe('GuardianOperatorHttpClient — account pause/unpause', () => {
  beforeEach(() => {
    vi.stubGlobal('fetch', mockFetch);
    mockFetch.mockReset();
  });
  afterEach(() => {
    vi.clearAllMocks();
    vi.unstubAllGlobals();
  });

  it('serializes paused=true into the listAccounts URL', async () => {
    mockFetch.mockResolvedValueOnce(okJson({ items: [], next_cursor: null }));
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listAccounts({ paused: true });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts?paused=true',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('serializes paused=false into the listAccounts URL', async () => {
    mockFetch.mockResolvedValueOnce(okJson({ items: [], next_cursor: null }));
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listAccounts({ paused: false });
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts?paused=false',
      expect.objectContaining({ method: 'GET' }),
    );
  });

  it('omits the paused query parameter when not provided', async () => {
    mockFetch.mockResolvedValueOnce(okJson({ items: [], next_cursor: null }));
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.listAccounts();
    const [calledUrl] = mockFetch.mock.calls[0]!;
    expect(String(calledUrl)).toBe('https://guardian.example/dashboard/accounts');
  });

  it('pauseAccount POSTs the reason and maps the response to camelCase', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xabc',
        before_state: 'active',
        after_state: 'paused',
        paused_at: '2026-05-19T14:30:00Z',
        paused_reason: 'compliance review',
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.pauseAccount('0xabc', 'compliance review');

    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xabc/pause',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ reason: 'compliance review' }),
      }),
    );
    expect(response).toEqual({
      accountId: '0xabc',
      beforeState: 'active',
      afterState: 'paused',
      pausedAt: '2026-05-19T14:30:00Z',
      pausedReason: 'compliance review',
    });
  });

  it('pauseAccount URL-encodes the account id', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xabc/def',
        before_state: 'active',
        after_state: 'paused',
        paused_at: '2026-05-19T14:30:00Z',
        paused_reason: 'r',
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    await client.pauseAccount('0xabc/def', 'r');
    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xabc%2Fdef/pause',
      expect.objectContaining({ method: 'POST' }),
    );
  });

  it('unpauseAccount POSTs without a body when reason is omitted', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xabc',
        before_state: 'paused',
        after_state: 'active',
        reason: null,
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.unpauseAccount('0xabc');

    const [calledUrl, calledInit] = mockFetch.mock.calls[0]!;
    expect(String(calledUrl)).toBe(
      'https://guardian.example/dashboard/accounts/0xabc/unpause',
    );
    expect((calledInit as RequestInit).method).toBe('POST');
    expect((calledInit as RequestInit).body).toBeUndefined();
    expect(response).toEqual({
      accountId: '0xabc',
      beforeState: 'paused',
      afterState: 'active',
      reason: null,
    });
  });

  it('unpauseAccount POSTs the reason when provided', async () => {
    mockFetch.mockResolvedValueOnce(
      okJson({
        account_id: '0xabc',
        before_state: 'paused',
        after_state: 'active',
        reason: 'cleared by legal',
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const response = await client.unpauseAccount('0xabc', 'cleared by legal');

    expect(mockFetch).toHaveBeenCalledWith(
      'https://guardian.example/dashboard/accounts/0xabc/unpause',
      expect.objectContaining({
        method: 'POST',
        body: JSON.stringify({ reason: 'cleared by legal' }),
      }),
    );
    expect(response.reason).toBe('cleared by legal');
  });

  it('normalizes a 409 GUARDIAN_ACCOUNT_PAUSED into code account_paused with paused details', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 409,
        statusText: 'Conflict',
        body: {
          success: false,
          code: 'GUARDIAN_ACCOUNT_PAUSED',
          error: 'account is paused',
          paused_at: '2026-05-19T14:30:00Z',
          paused_reason: 'compliance review',
          retryable: false,
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const err = (await client
      .pauseAccount('0xabc', 'reason')
      .catch((v) => v)) as GuardianOperatorHttpError;

    expect(err).toBeInstanceOf(GuardianOperatorHttpError);
    expect(err.status).toBe(409);
    expect(err.data?.code).toBe('account_paused');
    expect(err.data?.pausedAt).toBe('2026-05-19T14:30:00Z');
    expect(err.data?.pausedReason).toBe('compliance review');
    expect(err.data?.retryable).toBe(false);
  });

  it('account_paused tolerates a null paused_reason for forward compat', async () => {
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 409,
        statusText: 'Conflict',
        body: {
          success: false,
          code: 'GUARDIAN_ACCOUNT_PAUSED',
          error: 'account is paused',
          paused_at: '2026-05-19T14:30:00Z',
          paused_reason: null,
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const err = (await client
      .pauseAccount('0xabc', 'reason')
      .catch((v) => v)) as GuardianOperatorHttpError;

    expect(err.data?.code).toBe('account_paused');
    expect(err.data?.pausedReason).toBeNull();
  });

  it('drops parsed details when an account_paused error omits paused_at (contract drift)', async () => {
    // `tryParseErrorData` swallows parser failures and returns null,
    // so a malformed 409 body still surfaces a `GuardianOperatorHttpError`
    // — but without the normalized pause details that callers depend on.
    mockFetch.mockResolvedValueOnce(
      errorResponse({
        status: 409,
        statusText: 'Conflict',
        body: {
          success: false,
          code: 'GUARDIAN_ACCOUNT_PAUSED',
          error: 'account is paused',
        },
      }),
    );
    const client = new GuardianOperatorHttpClient('https://guardian.example');
    const err = (await client
      .pauseAccount('0xabc', 'reason')
      .catch((v) => v)) as GuardianOperatorHttpError;
    expect(err).toBeInstanceOf(GuardianOperatorHttpError);
    expect(err.status).toBe(409);
    expect(err.data).toBeNull();
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
