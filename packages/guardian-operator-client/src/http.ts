import type {
  DashboardAccountDetail,
  DashboardAccountResponse,
  DashboardAccountStateStatus,
  DashboardAccountsResponse,
  DashboardAccountSummary,
  GuardianOperatorHttpClientOptions,
  GuardianOperatorHttpErrorData,
  LogoutOperatorResponse,
  OperatorChallenge,
  OperatorChallengeResponse,
  VerifyOperatorRequest,
  VerifyOperatorResponse,
} from './types.js';

export class GuardianOperatorHttpError extends Error {
  readonly retryAfterSecs?: number;

  constructor(
    public readonly status: number,
    public readonly statusText: string,
    public readonly body: string,
    public readonly data: GuardianOperatorHttpErrorData | null,
  ) {
    super(
      `Guardian operator HTTP error ${status}: ${statusText}${
        data ? ` - ${data.error}` : body ? ` - ${body}` : ''
      }`,
    );
    this.name = 'GuardianOperatorHttpError';
    this.retryAfterSecs = data?.retryAfterSecs;
  }
}

export class GuardianOperatorContractError extends Error {
  constructor(
    public readonly context: string,
    message: string,
  ) {
    super(`${context}: ${message}`);
    this.name = 'GuardianOperatorContractError';
  }
}

export class GuardianOperatorHttpClient {
  private readonly baseUrl: URL;
  private readonly fetchImpl: typeof fetch;
  private readonly credentials?: RequestCredentials;
  private readonly defaultHeaders: Headers;

  constructor(baseUrl: string);
  constructor(options: GuardianOperatorHttpClientOptions);
  constructor(baseUrlOrOptions: string | GuardianOperatorHttpClientOptions) {
    const options =
      typeof baseUrlOrOptions === 'string'
        ? { baseUrl: baseUrlOrOptions }
        : baseUrlOrOptions;

    this.baseUrl = normalizeBaseUrl(options.baseUrl);
    this.fetchImpl = resolveFetch(options.fetch);
    this.credentials = options.credentials;
    this.defaultHeaders = new Headers(options.headers);
  }

  async challenge(commitment: string): Promise<OperatorChallengeResponse> {
    const url = new URL('auth/challenge', this.baseUrl);
    url.searchParams.set('commitment', commitment);
    return this.request(url, { method: 'GET' }, parseChallengeResponse);
  }

  async verify(request: VerifyOperatorRequest): Promise<VerifyOperatorResponse> {
    return this.request(
      new URL('auth/verify', this.baseUrl),
      {
        method: 'POST',
        body: JSON.stringify({
          commitment: request.commitment,
          signature: request.signature,
        }),
      },
      parseVerifyResponse,
    );
  }

  async logout(): Promise<LogoutOperatorResponse> {
    return this.request(
      new URL('auth/logout', this.baseUrl),
      { method: 'POST' },
      parseLogoutResponse,
    );
  }

  async listAccounts(): Promise<DashboardAccountsResponse> {
    return this.request(
      new URL('dashboard/accounts', this.baseUrl),
      { method: 'GET' },
      parseAccountsResponse,
    );
  }

  async getAccount(accountId: string): Promise<DashboardAccountResponse> {
    const encodedAccountId = encodeURIComponent(accountId);
    return this.request(
      new URL(`dashboard/accounts/${encodedAccountId}`, this.baseUrl),
      { method: 'GET' },
      parseAccountResponse,
    );
  }

  private async request<T>(
    url: URL,
    init: RequestInit,
    parse: (value: unknown) => T,
  ): Promise<T> {
    const response = await this.fetchImpl(url.toString(), {
      ...init,
      credentials: init.credentials ?? this.credentials,
      headers: buildHeaders(this.defaultHeaders, init),
    });

    if (!response.ok) {
      throw await this.toHttpError(response);
    }

    let payload: unknown;
    try {
      payload = await response.json();
    } catch (error) {
      throw new GuardianOperatorContractError(
        url.pathname,
        `expected JSON response: ${String(error)}`,
      );
    }

    return parse(payload);
  }

  private async toHttpError(response: Response): Promise<GuardianOperatorHttpError> {
    const body = await response.text();
    const data = tryParseErrorData(body);
    return new GuardianOperatorHttpError(
      response.status,
      response.statusText,
      body,
      data,
    );
  }
}

function normalizeBaseUrl(baseUrl: string): URL {
  const normalized = baseUrl.endsWith('/') ? baseUrl : `${baseUrl}/`;
  try {
    return new URL(normalized);
  } catch (error) {
    const documentBase = currentDocumentBase();
    if (!documentBase) {
      throw error;
    }
    return new URL(normalized, documentBase);
  }
}

function currentDocumentBase(): string | null {
  const location = globalThis.location;
  if (!location) {
    return null;
  }

  if (typeof location.href === 'string' && location.href.length > 0) {
    return location.href;
  }

  if (typeof location.origin === 'string' && location.origin.length > 0) {
    return `${location.origin}/`;
  }

  return null;
}

function resolveFetch(fetchImpl?: typeof fetch): typeof fetch {
  if (fetchImpl) {
    return ((input: RequestInfo | URL, init?: RequestInit) => fetchImpl(input, init)) as typeof fetch;
  }

  const globalFetch = globalThis.fetch;
  if (!globalFetch) {
    throw new Error('Fetch API is not available');
  }

  return globalFetch.bind(globalThis);
}

function buildHeaders(defaultHeaders: Headers, init: RequestInit): Headers {
  const headers = new Headers(defaultHeaders);
  headers.set('Accept', 'application/json');

  if (init.body !== undefined && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }

  const requestHeaders = new Headers(init.headers);
  requestHeaders.forEach((value, key) => {
    headers.set(key, value);
  });

  return headers;
}

function tryParseErrorData(body: string): GuardianOperatorHttpErrorData | null {
  if (!body) {
    return null;
  }

  try {
    return parseErrorResponse(JSON.parse(body));
  } catch {
    return null;
  }
}

function parseChallengeResponse(value: unknown): OperatorChallengeResponse {
  const record = asRecord(value, 'challenge response');
  return {
    success: requireSuccess(record, 'challenge response'),
    challenge: parseChallenge(requireField(record, 'challenge', 'challenge response')),
  };
}

function parseChallenge(value: unknown): OperatorChallenge {
  const record = asRecord(value, 'challenge');
  return {
    domain: requireString(record, 'domain', 'challenge'),
    commitment: requireString(record, 'commitment', 'challenge'),
    nonce: requireString(record, 'nonce', 'challenge'),
    expiresAt: requireString(record, 'expires_at', 'challenge'),
    signingDigest: requireString(record, 'signing_digest', 'challenge'),
  };
}

function parseVerifyResponse(value: unknown): VerifyOperatorResponse {
  const record = asRecord(value, 'verify response');
  return {
    success: requireSuccess(record, 'verify response'),
    operatorId: requireString(record, 'operator_id', 'verify response'),
    expiresAt: requireString(record, 'expires_at', 'verify response'),
  };
}

function parseLogoutResponse(value: unknown): LogoutOperatorResponse {
  const record = asRecord(value, 'logout response');
  return {
    success: requireSuccess(record, 'logout response'),
  };
}

function parseAccountsResponse(value: unknown): DashboardAccountsResponse {
  const record = asRecord(value, 'accounts response');
  return {
    success: requireSuccess(record, 'accounts response'),
    totalCount: requireInteger(record, 'total_count', 'accounts response'),
    accounts: requireArray(record, 'accounts', 'accounts response').map((entry, index) =>
      parseAccountSummary(entry, `accounts[${index}]`),
    ),
  };
}

function parseAccountResponse(value: unknown): DashboardAccountResponse {
  const record = asRecord(value, 'account response');
  return {
    success: requireSuccess(record, 'account response'),
    account: parseAccountDetail(requireField(record, 'account', 'account response'), 'account'),
  };
}

function parseAccountSummary(
  value: unknown,
  context: string,
): DashboardAccountSummary {
  const record = asRecord(value, context);
  return {
    accountId: requireString(record, 'account_id', context),
    authScheme: requireString(record, 'auth_scheme', context),
    authorizedSignerCount: requireInteger(record, 'authorized_signer_count', context),
    hasPendingCandidate: requireBoolean(record, 'has_pending_candidate', context),
    currentCommitment: requireNullableString(record, 'current_commitment', context),
    stateStatus: parseStateStatus(
      requireString(record, 'state_status', context),
      `${context}.state_status`,
    ),
    createdAt: requireString(record, 'created_at', context),
    updatedAt: requireString(record, 'updated_at', context),
  };
}

function parseAccountDetail(
  value: unknown,
  context: string,
): DashboardAccountDetail {
  const summary = parseAccountSummary(value, context);
  const record = asRecord(value, context);

  return {
    ...summary,
    authorizedSignerIds: requireStringArray(record, 'authorized_signer_ids', context),
    stateCreatedAt: requireNullableString(record, 'state_created_at', context),
    stateUpdatedAt: requireNullableString(record, 'state_updated_at', context),
  };
}

function parseErrorResponse(value: unknown): GuardianOperatorHttpErrorData {
  const record = asRecord(value, 'error response');
  const success = requireBoolean(record, 'success', 'error response');
  if (success) {
    throw new GuardianOperatorContractError(
      'error response',
      'expected success to be false',
    );
  }

  const retryAfterValue = record.retry_after_secs;
  let retryAfterSecs: number | undefined;
  if (retryAfterValue !== undefined) {
    if (typeof retryAfterValue !== 'number' || !Number.isInteger(retryAfterValue)) {
      throw new GuardianOperatorContractError(
        'error response',
        'retry_after_secs must be an integer when present',
      );
    }
    retryAfterSecs = retryAfterValue;
  }

  return {
    success: false,
    error: requireString(record, 'error', 'error response'),
    retryAfterSecs,
  };
}

function parseStateStatus(
  value: string,
  context: string,
): DashboardAccountStateStatus {
  if (value === 'available' || value === 'unavailable') {
    return value;
  }

  throw new GuardianOperatorContractError(
    context,
    `expected state_status to be "available" or "unavailable", got ${JSON.stringify(value)}`,
  );
}

function asRecord(value: unknown, context: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new GuardianOperatorContractError(context, 'expected an object');
  }

  return value as Record<string, unknown>;
}

function requireField(
  record: Record<string, unknown>,
  key: string,
  context: string,
): unknown {
  if (!(key in record)) {
    throw new GuardianOperatorContractError(context, `missing required field "${key}"`);
  }
  return record[key];
}

function requireString(
  record: Record<string, unknown>,
  key: string,
  context: string,
): string {
  const value = requireField(record, key, context);
  if (typeof value !== 'string') {
    throw new GuardianOperatorContractError(
      context,
      `field "${key}" must be a string`,
    );
  }
  return value;
}

function requireNullableString(
  record: Record<string, unknown>,
  key: string,
  context: string,
): string | null {
  const value = requireField(record, key, context);
  if (value === null) {
    return null;
  }
  if (typeof value !== 'string') {
    throw new GuardianOperatorContractError(
      context,
      `field "${key}" must be a string or null`,
    );
  }
  return value;
}

function requireBoolean(
  record: Record<string, unknown>,
  key: string,
  context: string,
): boolean {
  const value = requireField(record, key, context);
  if (typeof value !== 'boolean') {
    throw new GuardianOperatorContractError(
      context,
      `field "${key}" must be a boolean`,
    );
  }
  return value;
}

function requireSuccess(
  record: Record<string, unknown>,
  context: string,
): true {
  const value = requireBoolean(record, 'success', context);
  if (!value) {
    throw new GuardianOperatorContractError(context, 'expected success to be true');
  }
  return true;
}

function requireInteger(
  record: Record<string, unknown>,
  key: string,
  context: string,
): number {
  const value = requireField(record, key, context);
  if (typeof value !== 'number' || !Number.isInteger(value)) {
    throw new GuardianOperatorContractError(
      context,
      `field "${key}" must be an integer`,
    );
  }
  return value;
}

function requireArray(
  record: Record<string, unknown>,
  key: string,
  context: string,
): unknown[] {
  const value = requireField(record, key, context);
  if (!Array.isArray(value)) {
    throw new GuardianOperatorContractError(
      context,
      `field "${key}" must be an array`,
    );
  }
  return value;
}

function requireStringArray(
  record: Record<string, unknown>,
  key: string,
  context: string,
): string[] {
  return requireArray(record, key, context).map((entry, index) => {
    if (typeof entry !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        `field "${key}" entry ${index} must be a string`,
      );
    }
    return entry;
  });
}
