import type {
  DashboardAccountDetail,
  DashboardAccountResponse,
  DashboardAccountSnapshot,
  DashboardAccountStateStatus,
  DashboardAccountSummary,
  DashboardDeltaEntry,
  DashboardDeltaStatus,
  DashboardErrorCode,
  DashboardGlobalDeltaEntry,
  DashboardGlobalProposalEntry,
  DashboardInfoResponse,
  DashboardProposalEntry,
  DashboardVaultFungibleEntry,
  DashboardVaultNonFungibleEntry,
  DashboardVaultSnapshot,
  GlobalDeltasOptions,
  GuardianOperatorHttpClientOptions,
  GuardianOperatorHttpErrorData,
  LogoutOperatorResponse,
  OperatorChallenge,
  OperatorChallengeResponse,
  PagedResult,
  VerifyOperatorRequest,
  VerifyOperatorResponse,
} from './types.js';

/**
 * Common pagination query options for the new dashboard feeds
 * endpoints. Spec reference: `005-operator-dashboard-metrics` FR-001.
 */
export interface PaginationOptions {
  /** Page size; default 50 server-side, max 500. */
  limit?: number;
  /** Opaque cursor token from a prior page's `nextCursor`. */
  cursor?: string;
}

/**
 * Set of stable error codes added by feature
 * `005-operator-dashboard-metrics`. Used by {@link parseErrorBody} to
 * narrow the unknown server `code` to the typed
 * {@link DashboardErrorCode} union.
 */
const DASHBOARD_ERROR_CODES = new Set<DashboardErrorCode>([
  'authentication_failed',
  'account_not_found',
  'invalid_cursor',
  'invalid_limit',
  'invalid_status_filter',
  'data_unavailable',
]);

/**
 * Result of parsing an error response body. The {@link code} field is
 * narrowed to {@link DashboardErrorCode} when the server emitted one of
 * the dashboard error codes; otherwise it is forwarded as a raw string
 * (or `null` if the body was missing/malformed).
 */
export interface ParsedErrorBody {
  code: DashboardErrorCode | string | null;
  message: string | null;
  retryAfterSecs?: number;
}

/**
 * Parse an error response body into a typed `{ code, message }` shape.
 * Clients SHOULD branch on `code` rather than on the HTTP status alone
 * — see FR-028 of `005-operator-dashboard-metrics`.
 *
 * Accepts either a `Response` (fetch result) or a pre-read JSON body.
 * Returns `{ code: null, message: null }` when the body is absent or
 * malformed; never throws.
 */
export async function parseErrorBody(
  response: Response | unknown,
): Promise<ParsedErrorBody> {
  let raw: unknown;
  if (
    response &&
    typeof response === 'object' &&
    'text' in response &&
    typeof (response as Response).text === 'function'
  ) {
    const body = await (response as Response).text();
    if (!body) {
      return { code: null, message: null };
    }
    try {
      raw = JSON.parse(body);
    } catch {
      return { code: null, message: null };
    }
  } else {
    raw = response;
  }

  if (raw === null || typeof raw !== 'object') {
    return { code: null, message: null };
  }
  const record = raw as Record<string, unknown>;

  const codeRaw = record['code'];
  const code: DashboardErrorCode | string | null =
    typeof codeRaw === 'string' ? codeRaw : null;

  const messageRaw = record['error'];
  const message = typeof messageRaw === 'string' ? messageRaw : null;

  const retryRaw = record['retry_after_secs'];
  const retryAfterSecs =
    typeof retryRaw === 'number' && Number.isInteger(retryRaw)
      ? retryRaw
      : undefined;

  return { code, message, retryAfterSecs };
}

/**
 * Type guard narrowing an arbitrary string to {@link DashboardErrorCode}.
 */
export function isDashboardErrorCode(
  value: string,
): value is DashboardErrorCode {
  return DASHBOARD_ERROR_CODES.has(value as DashboardErrorCode);
}

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

  /**
   * Paginated account list per feature
   * `005-operator-dashboard-metrics` US1 / FR-001..FR-008.
   *
   * **Breaking change vs. `003-operator-account-apis`**: the response
   * is now a {@link PagedResult} envelope. The previous
   * unparameterized full-inventory mode and `total_count` field are
   * removed; aggregate inventory totals are exposed via
   * {@link GuardianOperatorHttpClient.getDashboardInfo}. Callers that
   * relied on `listAccounts()` must migrate to this method.
   */
  async listAccounts(
    options: PaginationOptions = {},
  ): Promise<PagedResult<DashboardAccountSummary>> {
    const url = new URL('dashboard/accounts', this.baseUrl);
    applyPaginationParams(url, options);
    return this.request(url, { method: 'GET' }, parseAccountListPage);
  }

  /**
   * Inventory and lifecycle health snapshot per feature
   * `005-operator-dashboard-metrics` US2 / FR-008..FR-012.
   */
  async getDashboardInfo(): Promise<DashboardInfoResponse> {
    return this.request(
      new URL('dashboard/info', this.baseUrl),
      { method: 'GET' },
      parseDashboardInfo,
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

  /**
   * Return a decoded snapshot of `accountId`'s stored state at the
   * commitment Guardian last canonicalized — v1 surface exposes the
   * fungible/non-fungible vault. Returns a `data_unavailable` error
   * for EVM accounts and for any Miden account whose state row is
   * missing or fails to decode.
   *
   * Spec reference: follow-up addition to `005-operator-dashboard-metrics`.
   */
  async getAccountSnapshot(accountId: string): Promise<DashboardAccountSnapshot> {
    const encodedAccountId = encodeURIComponent(accountId);
    return this.request(
      new URL(`dashboard/accounts/${encodedAccountId}/snapshot`, this.baseUrl),
      { method: 'GET' },
      parseAccountSnapshot,
    );
  }

  /**
   * List the per-account delta feed for `accountId`, paginated
   * newest-first by `nonce DESC`. Spec reference:
   * `005-operator-dashboard-metrics` US3.
   */
  async listAccountDeltas(
    accountId: string,
    options: PaginationOptions = {},
  ): Promise<PagedResult<DashboardDeltaEntry>> {
    const encodedAccountId = encodeURIComponent(accountId);
    const url = new URL(
      `dashboard/accounts/${encodedAccountId}/deltas`,
      this.baseUrl,
    );
    applyPaginationParams(url, options);
    return this.request(url, { method: 'GET' }, parseDeltaPage);
  }

  /**
   * List the per-account in-flight multisig proposal queue for
   * `accountId`, paginated newest-first by `(nonce DESC, commitment
   * DESC)`. Single-key Miden and EVM accounts always return an empty
   * page per FR-017. Spec reference:
   * `005-operator-dashboard-metrics` US4.
   */
  async listAccountProposals(
    accountId: string,
    options: PaginationOptions = {},
  ): Promise<PagedResult<DashboardProposalEntry>> {
    const encodedAccountId = encodeURIComponent(accountId);
    const url = new URL(
      `dashboard/accounts/${encodedAccountId}/proposals`,
      this.baseUrl,
    );
    applyPaginationParams(url, options);
    return this.request(url, { method: 'GET' }, parseProposalPage);
  }

  /**
   * Cross-account delta feed. Paginated newest-first by
   * `status_timestamp DESC`. The optional `status` filter accepts a
   * single status or an array; the wrapper serializes the array to
   * comma-separated. Spec reference:
   * `005-operator-dashboard-metrics` US6.
   */
  async listGlobalDeltas(
    options: GlobalDeltasOptions = {},
  ): Promise<PagedResult<DashboardGlobalDeltaEntry>> {
    const url = new URL('dashboard/deltas', this.baseUrl);
    if (options.limit !== undefined) {
      url.searchParams.set('limit', String(options.limit));
    }
    if (options.cursor !== undefined) {
      url.searchParams.set('cursor', options.cursor);
    }
    if (options.status !== undefined) {
      const statuses = Array.isArray(options.status)
        ? options.status
        : [options.status];
      if (statuses.length > 0) {
        url.searchParams.set('status', statuses.join(','));
      }
    }
    return this.request(url, { method: 'GET' }, parseGlobalDeltasPage);
  }

  /**
   * Cross-account in-flight proposal feed. Paginated newest-first by
   * `originating_timestamp DESC`. Takes no `status` filter — every
   * entry is in-flight by definition. EVM accounts do not appear in
   * v1 per FR-017. Spec reference:
   * `005-operator-dashboard-metrics` US7.
   */
  async listGlobalProposals(
    options: PaginationOptions = {},
  ): Promise<PagedResult<DashboardGlobalProposalEntry>> {
    const url = new URL('dashboard/proposals', this.baseUrl);
    applyPaginationParams(url, options);
    return this.request(url, { method: 'GET' }, parseGlobalProposalsPage);
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

function parseAccountListPage(
  value: unknown,
): PagedResult<DashboardAccountSummary> {
  return parsePagedResult(value, parseAccountSummary, 'accounts page');
}

function parseDashboardInfo(value: unknown): DashboardInfoResponse {
  const record = asRecord(value, 'dashboard info');
  const serviceStatusRaw = requireString(
    record,
    'service_status',
    'dashboard info',
  );
  let serviceStatus: 'healthy' | 'degraded';
  if (serviceStatusRaw === 'healthy' || serviceStatusRaw === 'degraded') {
    serviceStatus = serviceStatusRaw;
  } else {
    throw new GuardianOperatorContractError(
      'dashboard info',
      `expected service_status to be "healthy" or "degraded", got ${JSON.stringify(serviceStatusRaw)}`,
    );
  }

  const counts = asRecord(
    requireField(record, 'delta_status_counts', 'dashboard info'),
    'dashboard info.delta_status_counts',
  );

  const buildRecord = asRecord(
    requireField(record, 'build', 'dashboard info'),
    'dashboard info.build',
  );
  const profileRaw = requireString(buildRecord, 'profile', 'dashboard info.build');
  if (profileRaw !== 'debug' && profileRaw !== 'release') {
    throw new GuardianOperatorContractError(
      'dashboard info.build',
      `expected profile to be "debug" or "release", got ${JSON.stringify(profileRaw)}`,
    );
  }
  const build: DashboardInfoResponse['build'] = {
    version: requireString(buildRecord, 'version', 'dashboard info.build'),
    gitCommit: requireString(buildRecord, 'git_commit', 'dashboard info.build'),
    profile: profileRaw,
    startedAt: requireString(buildRecord, 'started_at', 'dashboard info.build'),
  };

  const backendRecord = asRecord(
    requireField(record, 'backend', 'dashboard info'),
    'dashboard info.backend',
  );
  const storageRaw = requireString(backendRecord, 'storage', 'dashboard info.backend');
  if (storageRaw !== 'filesystem' && storageRaw !== 'postgres') {
    throw new GuardianOperatorContractError(
      'dashboard info.backend',
      `expected storage to be "filesystem" or "postgres", got ${JSON.stringify(storageRaw)}`,
    );
  }
  const canonicalizationField = backendRecord['canonicalization'];
  let canonicalization: DashboardInfoResponse['backend']['canonicalization'];
  if (canonicalizationField === null || canonicalizationField === undefined) {
    canonicalization = null;
  } else {
    const c = asRecord(canonicalizationField, 'dashboard info.backend.canonicalization');
    canonicalization = {
      checkIntervalSeconds: requireInteger(
        c,
        'check_interval_seconds',
        'dashboard info.backend.canonicalization',
      ),
      maxRetries: requireInteger(
        c,
        'max_retries',
        'dashboard info.backend.canonicalization',
      ),
      submissionGracePeriodSeconds: requireInteger(
        c,
        'submission_grace_period_seconds',
        'dashboard info.backend.canonicalization',
      ),
    };
  }
  const backend: DashboardInfoResponse['backend'] = {
    storage: storageRaw,
    supportedAckSchemes: requireStringArray(
      backendRecord,
      'supported_ack_schemes',
      'dashboard info.backend',
    ),
    canonicalization,
  };

  const authMethodsRecord = asRecord(
    requireField(record, 'accounts_by_auth_method', 'dashboard info'),
    'dashboard info.accounts_by_auth_method',
  );
  const accountsByAuthMethod: Record<string, number> = {};
  for (const [key, raw] of Object.entries(authMethodsRecord)) {
    if (typeof raw !== 'number' || !Number.isInteger(raw) || raw < 0) {
      throw new GuardianOperatorContractError(
        'dashboard info.accounts_by_auth_method',
        `expected non-negative integer count for "${key}", got ${JSON.stringify(raw)}`,
      );
    }
    accountsByAuthMethod[key] = raw;
  }

  return {
    serviceStatus,
    environment: requireString(record, 'environment', 'dashboard info'),
    build,
    backend,
    totalAccountCount: requireInteger(
      record,
      'total_account_count',
      'dashboard info',
    ),
    accountsByAuthMethod,
    latestActivity: requireNullableString(
      record,
      'latest_activity',
      'dashboard info',
    ),
    deltaStatusCounts: {
      candidate: requireInteger(
        counts,
        'candidate',
        'dashboard info.delta_status_counts',
      ),
      canonical: requireInteger(
        counts,
        'canonical',
        'dashboard info.delta_status_counts',
      ),
      discarded: requireInteger(
        counts,
        'discarded',
        'dashboard info.delta_status_counts',
      ),
    },
    inFlightProposalCount: requireInteger(
      record,
      'in_flight_proposal_count',
      'dashboard info',
    ),
    degradedAggregates: requireStringArray(
      record,
      'degraded_aggregates',
      'dashboard info',
    ),
  };
}

function parseAccountResponse(value: unknown): DashboardAccountResponse {
  return parseAccountDetail(value, 'account response');
}

function parseAccountSnapshot(value: unknown): DashboardAccountSnapshot {
  const record = asRecord(value, 'account snapshot');
  const vaultRecord = asRecord(
    requireField(record, 'vault', 'account snapshot'),
    'account snapshot.vault',
  );
  const fungibleRaw = requireArray(
    vaultRecord,
    'fungible',
    'account snapshot.vault',
  );
  const nonFungibleRaw = requireArray(
    vaultRecord,
    'non_fungible',
    'account snapshot.vault',
  );
  const fungible: DashboardVaultFungibleEntry[] = fungibleRaw.map((entry, idx) => {
    const ctx = `account snapshot.vault.fungible[${idx}]`;
    const r = asRecord(entry, ctx);
    return {
      faucetId: requireString(r, 'faucet_id', ctx),
      amount: requireString(r, 'amount', ctx),
    };
  });
  const nonFungible: DashboardVaultNonFungibleEntry[] = nonFungibleRaw.map((entry, idx) => {
    const ctx = `account snapshot.vault.non_fungible[${idx}]`;
    const r = asRecord(entry, ctx);
    return {
      faucetId: requireString(r, 'faucet_id', ctx),
      vaultKey: requireString(r, 'vault_key', ctx),
    };
  });
  const vault: DashboardVaultSnapshot = { fungible, nonFungible };
  return {
    commitment: requireString(record, 'commitment', 'account snapshot'),
    updatedAt: requireString(record, 'updated_at', 'account snapshot'),
    hasPendingCandidate: requireBoolean(
      record,
      'has_pending_candidate',
      'account snapshot',
    ),
    vault,
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

  // Optional stable machine-readable error code added by feature
  // `005-operator-dashboard-metrics` and required for the dashboard
  // error taxonomy (FR-028). Older servers may omit it; tolerate that
  // by leaving the field undefined.
  const codeValue = record.code;
  let code: string | undefined;
  if (codeValue !== undefined) {
    if (typeof codeValue !== 'string') {
      throw new GuardianOperatorContractError(
        'error response',
        'code must be a string when present',
      );
    }
    code = codeValue;
  }

  return {
    success: false,
    code,
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

// ---------------------------------------------------------------------------
// Pagination helpers — feature `005-operator-dashboard-metrics`.
// ---------------------------------------------------------------------------

function applyPaginationParams(url: URL, options: PaginationOptions): void {
  if (options.limit !== undefined) {
    url.searchParams.set('limit', String(options.limit));
  }
  if (options.cursor !== undefined) {
    url.searchParams.set('cursor', options.cursor);
  }
}

function parseDeltaPage(
  value: unknown,
): PagedResult<DashboardDeltaEntry> {
  return parsePagedResult(value, parseDeltaEntry, 'deltas page');
}

function parseProposalPage(
  value: unknown,
): PagedResult<DashboardProposalEntry> {
  return parsePagedResult(value, parseProposalEntry, 'proposals page');
}

function parseGlobalDeltasPage(
  value: unknown,
): PagedResult<DashboardGlobalDeltaEntry> {
  return parsePagedResult(
    value,
    (entry, ctx) => parseGlobalDeltaEntry(entry, ctx),
    'global deltas page',
  );
}

function parseGlobalProposalsPage(
  value: unknown,
): PagedResult<DashboardGlobalProposalEntry> {
  return parsePagedResult(
    value,
    (entry, ctx) => parseGlobalProposalEntry(entry, ctx),
    'global proposals page',
  );
}

function parseGlobalDeltaEntry(
  value: unknown,
  context: string,
): DashboardGlobalDeltaEntry {
  const base = parseDeltaEntry(value, context);
  if (!base.accountId) {
    throw new GuardianOperatorContractError(
      context,
      'global delta feed entries must include account_id',
    );
  }
  return base as DashboardGlobalDeltaEntry;
}

function parseGlobalProposalEntry(
  value: unknown,
  context: string,
): DashboardGlobalProposalEntry {
  const base = parseProposalEntry(value, context);
  if (!base.accountId) {
    throw new GuardianOperatorContractError(
      context,
      'global proposal feed entries must include account_id',
    );
  }
  return base as DashboardGlobalProposalEntry;
}

function parsePagedResult<T>(
  value: unknown,
  parseItem: (entry: unknown, context: string) => T,
  context: string,
): PagedResult<T> {
  const record = asRecord(value, context);
  const items = requireArray(record, 'items', context).map((entry, index) =>
    parseItem(entry, `${context}.items[${index}]`),
  );
  const nextCursorRaw = requireField(record, 'next_cursor', context);
  let nextCursor: string | null;
  if (nextCursorRaw === null) {
    nextCursor = null;
  } else if (typeof nextCursorRaw === 'string') {
    nextCursor = nextCursorRaw;
  } else {
    throw new GuardianOperatorContractError(
      context,
      'next_cursor must be a string or null',
    );
  }
  return { items, nextCursor };
}

function parseDeltaEntry(
  value: unknown,
  context: string,
): DashboardDeltaEntry {
  const record = asRecord(value, context);
  const entry: DashboardDeltaEntry = {
    nonce: requireInteger(record, 'nonce', context),
    status: parseDeltaStatus(
      requireString(record, 'status', context),
      `${context}.status`,
    ),
    statusTimestamp: requireString(record, 'status_timestamp', context),
    prevCommitment: requireString(record, 'prev_commitment', context),
    newCommitment: requireNullableString(record, 'new_commitment', context),
  };
  if (record.retry_count !== undefined) {
    const retry = record.retry_count;
    if (typeof retry !== 'number' || !Number.isInteger(retry) || retry < 0) {
      throw new GuardianOperatorContractError(
        context,
        'retry_count must be a non-negative integer when present',
      );
    }
    entry.retryCount = retry;
  }
  if (record.account_id !== undefined) {
    if (typeof record.account_id !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        'account_id must be a string when present',
      );
    }
    entry.accountId = record.account_id;
  }
  if (record.proposal_type !== undefined) {
    if (typeof record.proposal_type !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        'proposal_type must be a string when present',
      );
    }
    entry.proposalType = record.proposal_type;
  }
  return entry;
}

function parseProposalEntry(
  value: unknown,
  context: string,
): DashboardProposalEntry {
  const record = asRecord(value, context);
  const entry: DashboardProposalEntry = {
    commitment: requireString(record, 'commitment', context),
    nonce: requireInteger(record, 'nonce', context),
    proposerId: requireString(record, 'proposer_id', context),
    originatingTimestamp: requireString(
      record,
      'originating_timestamp',
      context,
    ),
    signaturesCollected: requireInteger(
      record,
      'signatures_collected',
      context,
    ),
    signaturesRequired: requireInteger(
      record,
      'signatures_required',
      context,
    ),
    prevCommitment: requireString(record, 'prev_commitment', context),
    newCommitment: requireNullableString(record, 'new_commitment', context),
  };
  if (record.account_id !== undefined) {
    if (typeof record.account_id !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        'account_id must be a string when present',
      );
    }
    entry.accountId = record.account_id;
  }
  if (record.proposal_type !== undefined) {
    if (typeof record.proposal_type !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        'proposal_type must be a string when present',
      );
    }
    entry.proposalType = record.proposal_type;
  }
  return entry;
}

function parseDeltaStatus(
  value: string,
  context: string,
): DashboardDeltaStatus {
  if (value === 'candidate' || value === 'canonical' || value === 'discarded') {
    return value;
  }
  throw new GuardianOperatorContractError(
    context,
    `expected status to be "candidate" / "canonical" / "discarded", got ${JSON.stringify(value)}`,
  );
}
