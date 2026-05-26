import type {
  AccountStatus,
  DashboardAccountDetail,
  DashboardAccountResponse,
  DashboardAccountSnapshot,
  DashboardAccountStateStatus,
  DashboardAccountSummary,
  DashboardDeltaAssetSummary,
  DashboardDeltaCategory,
  DashboardDeltaCounterpartySummary,
  DashboardDeltaDecodeSection,
  DashboardDeltaDecodeWarning,
  DashboardDeltaDecodedAsset,
  DashboardDeltaDecodedNote,
  DashboardDeltaDetail,
  DashboardDeltaEntry,
  DashboardDeltaNoteCounts,
  DashboardDeltaNoteTag,
  DashboardDeltaProposalMetadata,
  DashboardDeltaStatus,
  DashboardDeltaStorageChange,
  DashboardDeltaVaultChange,
  DeltaAssetKind,
  DeltaCounterpartyDirection,
  DashboardErrorCode,
  DashboardGlobalDeltaEntry,
  DashboardGlobalProposalEntry,
  DashboardInfoResponse,
  DashboardProposalEntry,
  DashboardVaultFungibleEntry,
  DashboardVaultNonFungibleEntry,
  DashboardVaultSnapshot,
  GlobalDeltasOptions,
  DeltaDetailOptions,
  GuardianOperatorHttpClientOptions,
  GuardianOperatorHttpErrorData,
  LogoutOperatorResponse,
  OperatorChallenge,
  OperatorChallengeResponse,
  PagedResult,
  PauseAccountResponse,
  SessionInfoResponse,
  UnpauseAccountResponse,
  VerifyOperatorRequest,
  VerifyOperatorResponse,
} from './types.js';
import type { OperatorPermission } from './permissions.js';

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
 * Options for {@link GuardianOperatorHttpClient.listAccounts}. Extends
 * {@link PaginationOptions} with a tri-state pause filter: `true` for
 * paused accounts only, `false` for active only, omitted for all.
 */
export interface ListAccountsOptions extends PaginationOptions {
  paused?: boolean;
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
  // Snapshot-specific codes (FR-045).
  'unsupported_for_network',
  'account_data_unavailable',
  // Server emits SCREAMING_SNAKE_CASE; the TS union surfaces snake_case
  // to match the rest of the DashboardErrorCode vocabulary.
  // `parseErrorBody` does the mapping at the boundary.
  'insufficient_operator_permission',
  // Same wire-form / TS-form mapping as `insufficient_operator_permission`.
  'account_paused',
]);

/** Server-emitted wire form for the permission-denial error code. */
const WIRE_INSUFFICIENT_OPERATOR_PERMISSION =
  'GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION';

/** Server-emitted wire form for the account-paused error code. */
const WIRE_ACCOUNT_PAUSED = 'GUARDIAN_ACCOUNT_PAUSED';

/**
 * Map a server-emitted error `code` to the typed
 * {@link DashboardErrorCode} surface. The only mapping today is for
 * the permission-denial code; every other code matches by string
 * equality.
 */
function mapDashboardErrorCode(raw: string | null): string | null {
  if (raw === WIRE_INSUFFICIENT_OPERATOR_PERMISSION) {
    return 'insufficient_operator_permission';
  }
  if (raw === WIRE_ACCOUNT_PAUSED) {
    return 'account_paused';
  }
  return raw;
}

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
  /**
   * Populated only when `code === 'insufficient_operator_permission'`.
   * Lists the permission strings the route required that the
   * authenticated operator does not hold, lexicographically sorted by
   * the server (feature 006-operator-authz FR-017).
   */
  missingPermissions?: readonly string[];
  /**
   * `false` for permission denials and account-paused rejections;
   * absent for every other code.
   */
  retryable?: boolean;
  /**
   * Populated only when `code === 'account_paused'`. RFC 3339 UTC
   * timestamp of the original pause.
   */
  pausedAt?: string;
  /**
   * Populated only when `code === 'account_paused'`. May be `null`
   * for forward compatibility (the v1 server always emits a
   * non-null reason).
   */
  pausedReason?: string | null;
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
    // `response.text()` itself can reject when the body has already
    // been consumed or the underlying stream errors. The function's
    // contract says we never throw, so swallow that and treat it the
    // same as an empty body.
    let body: string;
    try {
      body = await (response as Response).text();
    } catch {
      return { code: null, message: null };
    }
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
  const code: DashboardErrorCode | string | null = mapDashboardErrorCode(
    typeof codeRaw === 'string' ? codeRaw : null,
  );

  const messageRaw = record['error'];
  const message = typeof messageRaw === 'string' ? messageRaw : null;

  const retryRaw = record['retry_after_secs'];
  const retryAfterSecs =
    typeof retryRaw === 'number' && Number.isInteger(retryRaw)
      ? retryRaw
      : undefined;

  // Feature 006-operator-authz FR-016: populate `missingPermissions`
  // and `retryable` only when the server emitted the permission-denial
  // code. Every other code path leaves both fields undefined so the
  // additive envelope extension is invisible to existing parsers. The
  // contract pins `retryable=false` for this code; surface that
  // unconditionally so a non-compliant server can't mislead retry
  // policy code into retrying a permission denial.
  let missingPermissions: readonly string[] | undefined;
  let retryable: boolean | undefined;
  let pausedAt: string | undefined;
  let pausedReason: string | null | undefined;
  if (code === 'insufficient_operator_permission') {
    const missingRaw = record['missing_permissions'];
    if (
      Array.isArray(missingRaw) &&
      missingRaw.every((v): v is string => typeof v === 'string')
    ) {
      missingPermissions = missingRaw as readonly string[];
    }
    retryable = false;
  } else if (code === 'account_paused') {
    const pausedAtRaw = record['paused_at'];
    if (typeof pausedAtRaw === 'string') {
      pausedAt = pausedAtRaw;
    }
    const reasonRaw = record['paused_reason'];
    if (typeof reasonRaw === 'string') {
      pausedReason = reasonRaw;
    } else if (reasonRaw === null) {
      pausedReason = null;
    }
    retryable = false;
  }

  return {
    code,
    message,
    retryAfterSecs,
    missingPermissions,
    retryable,
    pausedAt,
    pausedReason,
  };
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
    options: ListAccountsOptions = {},
  ): Promise<PagedResult<DashboardAccountSummary>> {
    const url = new URL('dashboard/accounts', this.baseUrl);
    applyPaginationParams(url, options);
    if (options.paused !== undefined) {
      url.searchParams.set('paused', String(options.paused));
    }
    return this.request(url, { method: 'GET' }, parseAccountListPage);
  }

  /**
   * Return the authenticated operator's identity and effective
   * permission set. Succeeds for any valid session, including
   * operators with `permissions: []` (returns an empty array).
   */
  async getSession(): Promise<SessionInfoResponse> {
    return this.request(
      new URL('dashboard/session', this.baseUrl),
      { method: 'GET' },
      parseSessionInfo,
    );
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

  /**
   * Pause mutating actions for the given account. Requires
   * `accounts:pause`. Idempotent: re-pausing an already-paused account
   * returns success with the original `pausedAt` preserved.
   */
  async pauseAccount(
    accountId: string,
    reason: string,
  ): Promise<PauseAccountResponse> {
    const encoded = encodeURIComponent(accountId);
    return this.request(
      new URL(`dashboard/accounts/${encoded}/pause`, this.baseUrl),
      { method: 'POST', body: JSON.stringify({ reason }) },
      parsePauseResponse,
    );
  }

  /**
   * Clear pause state for the given account. Requires `accounts:pause`.
   * Idempotent: unpausing an already-active account returns success
   * with no state change.
   */
  async unpauseAccount(
    accountId: string,
    reason?: string,
  ): Promise<UnpauseAccountResponse> {
    const encoded = encodeURIComponent(accountId);
    const init: RequestInit = { method: 'POST' };
    if (reason !== undefined) {
      init.body = JSON.stringify({ reason });
    }
    return this.request(
      new URL(`dashboard/accounts/${encoded}/unpause`, this.baseUrl),
      init,
      parseUnpauseResponse,
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
   * fungible/non-fungible vault. The endpoint distinguishes its
   * failure modes via the FR-045 error taxonomy:
   *
   * - `400 unsupported_for_network` — the account's `network_config`
   *   is EVM. The snapshot endpoint is Miden-only by construction;
   *   this is a permanent condition for the account on this surface,
   *   not a transient failure.
   * - `404 account_not_found` — no metadata exists.
   * - `503 account_data_unavailable` — metadata exists but the state
   *   row cannot be loaded, or the stored blob fails to deserialize
   *   as a Miden `Account`. Transient/recoverable.
   *
   * Spec reference: follow-up addition to `005-operator-dashboard-metrics`
   * FR-043..FR-046.
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
   * Fetch the full detail projection of one canonical delta. The
   * nonce is serialized as a canonical base-10 `u64` URL segment;
   * unknown account and unknown nonce both surface as
   * `404 delta_not_found`.
   */
  async getAccountDeltaDetail(
    accountId: string,
    nonce: number,
    options: DeltaDetailOptions = {},
  ): Promise<DashboardDeltaDetail> {
    if (!Number.isSafeInteger(nonce) || nonce < 0) {
      throw new GuardianOperatorContractError(
        'getAccountDeltaDetail.nonce',
        `nonce must be a non-negative safe integer, got ${nonce}`,
      );
    }
    const encodedAccountId = encodeURIComponent(accountId);
    const url = new URL(
      `dashboard/accounts/${encodedAccountId}/deltas/${nonce.toString()}`,
      this.baseUrl,
    );
    if (options.includeRaw) {
      url.searchParams.set('include', 'raw');
    }
    return this.request(url, { method: 'GET' }, parseDeltaDetail);
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

function parseSessionInfo(value: unknown): SessionInfoResponse {
  const record = asRecord(value, 'session response');
  const permissions = requireStringArray(
    record,
    'permissions',
    'session response',
  );
  for (const [index, permission] of permissions.entries()) {
    if (!KNOWN_OPERATOR_PERMISSIONS.has(permission)) {
      throw new GuardianOperatorContractError(
        'session response',
        `permissions[${index}] is not a known operator permission: ${JSON.stringify(permission)}`,
      );
    }
  }
  return {
    operatorId: requireString(record, 'operator_id', 'session response'),
    permissions: permissions as OperatorPermission[],
  };
}

/**
 * Stable v1 operator permission vocabulary. Mirrors
 * `crates/server/src/dashboard/permissions.rs::Permission::as_str`.
 * Surfaces server/client drift as a contract failure instead of
 * silently flowing unknown strings through.
 */
const KNOWN_OPERATOR_PERMISSIONS: ReadonlySet<string> = new Set<OperatorPermission>([
  'dashboard:read',
  'accounts:pause',
  'policies:write',
]);

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
  const summary: DashboardAccountSummary = {
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
    pausedAt: requireNullableString(record, 'paused_at', context),
    pausedReason: requireNullableString(record, 'paused_reason', context),
  };
  if (record.account_id_bech32 !== undefined && record.account_id_bech32 !== null) {
    if (typeof record.account_id_bech32 !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        'account_id_bech32 must be a string when present',
      );
    }
    summary.accountIdBech32 = record.account_id_bech32;
  }
  return summary;
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

function parseAccountStatus(value: string, context: string): AccountStatus {
  if (value === 'active' || value === 'paused') {
    return value;
  }
  throw new GuardianOperatorContractError(
    context,
    `expected account status "active" or "paused", got ${JSON.stringify(value)}`,
  );
}

function parsePauseResponse(value: unknown): PauseAccountResponse {
  const ctx = 'pause response';
  const record = asRecord(value, ctx);
  return {
    accountId: requireString(record, 'account_id', ctx),
    beforeState: parseAccountStatus(
      requireString(record, 'before_state', ctx),
      `${ctx}.before_state`,
    ),
    afterState: parseAccountStatus(
      requireString(record, 'after_state', ctx),
      `${ctx}.after_state`,
    ),
    pausedAt: requireString(record, 'paused_at', ctx),
    pausedReason: requireString(record, 'paused_reason', ctx),
  };
}

function parseUnpauseResponse(value: unknown): UnpauseAccountResponse {
  const ctx = 'unpause response';
  const record = asRecord(value, ctx);
  return {
    accountId: requireString(record, 'account_id', ctx),
    beforeState: parseAccountStatus(
      requireString(record, 'before_state', ctx),
      `${ctx}.before_state`,
    ),
    afterState: parseAccountStatus(
      requireString(record, 'after_state', ctx),
      `${ctx}.after_state`,
    ),
    reason: requireNullableString(record, 'reason', ctx),
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
    code = mapDashboardErrorCode(codeValue) ?? undefined;
  }

  // Feature 006-operator-authz FR-016: populate `missingPermissions`
  // and `retryable` only on the permission-denial code. Tolerate
  // either ordering or omission on every other code so legacy 5xx /
  // 4xx errors continue to parse byte-for-byte as before.
  let missingPermissions: readonly string[] | undefined;
  let retryable: boolean | undefined;
  let pausedAt: string | undefined;
  let pausedReason: string | null | undefined;
  if (code === 'insufficient_operator_permission') {
    const missingRaw = record.missing_permissions;
    if (missingRaw !== undefined) {
      if (
        !Array.isArray(missingRaw) ||
        !missingRaw.every((v): v is string => typeof v === 'string')
      ) {
        throw new GuardianOperatorContractError(
          'error response',
          'missing_permissions must be an array of strings',
        );
      }
      missingPermissions = missingRaw as readonly string[];
    }
    const retryableRaw = record.retryable;
    if (retryableRaw !== undefined) {
      if (retryableRaw !== false) {
        // FR-016 pins `retryable: false` for permission denials.
        // Surface server contract drift loudly rather than letting
        // retry policy code retry an unretryable failure.
        throw new GuardianOperatorContractError(
          'error response',
          'retryable must be false for insufficient_operator_permission',
        );
      }
      retryable = false;
    }
  } else if (code === 'account_paused') {
    const pausedAtRaw = record.paused_at;
    if (typeof pausedAtRaw !== 'string') {
      throw new GuardianOperatorContractError(
        'error response',
        'paused_at must be a string for account_paused',
      );
    }
    pausedAt = pausedAtRaw;
    const reasonRaw = record.paused_reason;
    if (typeof reasonRaw === 'string' || reasonRaw === null) {
      pausedReason = reasonRaw;
    } else if (reasonRaw !== undefined) {
      throw new GuardianOperatorContractError(
        'error response',
        'paused_reason must be a string or null for account_paused',
      );
    }
    const retryableRaw = record.retryable;
    if (retryableRaw !== undefined && retryableRaw !== false) {
      throw new GuardianOperatorContractError(
        'error response',
        'retryable must be false for account_paused',
      );
    }
    retryable = false;
  }

  return {
    success: false,
    code,
    error: requireString(record, 'error', 'error response'),
    retryAfterSecs,
    missingPermissions,
    retryable,
    pausedAt,
    pausedReason,
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

function requireNonNegativeInteger(
  value: unknown,
  key: string,
  context: string,
): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) {
    throw new GuardianOperatorContractError(
      context,
      `field "${key}" must be a non-negative integer`,
    );
  }
  return value;
}

function assertStringArray(
  value: unknown[],
  key: string,
  context: string,
): string[] {
  value.forEach((entry, index) => {
    if (typeof entry !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        `field "${key}" entry ${index} must be a string`,
      );
    }
  });
  return value as string[];
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
  if (record.category !== undefined && record.category !== null) {
    entry.category = parseDeltaCategory(
      requireString(record, 'category', context),
      `${context}.category`,
    );
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
  if (record.note_counts !== undefined && record.note_counts !== null) {
    entry.noteCounts = parseDeltaNoteCounts(
      requireField(record, 'note_counts', context),
      `${context}.note_counts`,
    );
  }
  if (record.assets !== undefined) {
    entry.assets = parseDeltaAssetSummaryArray(
      record.assets,
      `${context}.assets`,
    );
  }
  if (record.counterparty !== undefined && record.counterparty !== null) {
    entry.counterparty = parseDeltaCounterpartySummary(
      record.counterparty,
      `${context}.counterparty`,
    );
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

const DELTA_CATEGORY_VALUES: readonly DashboardDeltaCategory[] = [
  'asset_transfer',
  'note_consumption',
  'note_creation',
  'account_storage_change',
  'guardian_switch',
  'custom',
];

function parseDeltaCategory(
  value: string,
  context: string,
): DashboardDeltaCategory {
  if ((DELTA_CATEGORY_VALUES as readonly string[]).includes(value)) {
    return value as DashboardDeltaCategory;
  }
  throw new GuardianOperatorContractError(
    context,
    `invalid category "${value}" — expected one of ${DELTA_CATEGORY_VALUES.join(', ')}`,
  );
}

function parseDeltaAssetSummary(
  value: unknown,
  context: string,
): DashboardDeltaAssetSummary {
  const record = asRecord(value, context);
  const kind = requireString(record, 'kind', context);
  if (kind !== 'fungible' && kind !== 'non_fungible') {
    throw new GuardianOperatorContractError(
      context,
      `asset kind must be "fungible" or "non_fungible", got "${kind}"`,
    );
  }
  const asset: DashboardDeltaAssetSummary = {
    assetId: requireString(record, 'asset_id', context),
    kind: kind as DeltaAssetKind,
  };
  if (record.amount !== undefined && record.amount !== null) {
    if (typeof record.amount !== 'string') {
      throw new GuardianOperatorContractError(
        context,
        'asset amount must be a string when present',
      );
    }
    asset.amount = record.amount;
  }
  return asset;
}

function parseDeltaAssetSummaryArray(
  value: unknown,
  context: string,
): DashboardDeltaAssetSummary[] {
  if (!Array.isArray(value)) {
    throw new GuardianOperatorContractError(context, 'expected an array');
  }
  return value.map((item, index) =>
    parseDeltaAssetSummary(item, `${context}[${index}]`),
  );
}

function parseDeltaCounterpartySummary(
  value: unknown,
  context: string,
): DashboardDeltaCounterpartySummary {
  const record = asRecord(value, context);
  const direction = requireString(record, 'direction', context);
  if (direction !== 'in' && direction !== 'out') {
    throw new GuardianOperatorContractError(
      context,
      `counterparty direction must be "in" or "out", got "${direction}"`,
    );
  }
  return {
    accountId: requireString(record, 'account_id', context),
    direction: direction as DeltaCounterpartyDirection,
  };
}

function parseDeltaNoteCounts(
  value: unknown,
  context: string,
): DashboardDeltaNoteCounts {
  const record = asRecord(value, context);
  const input = requireInteger(record, 'input', context);
  const output = requireInteger(record, 'output', context);
  if (input < 0 || output < 0) {
    throw new GuardianOperatorContractError(
      context,
      'note counts must be non-negative integers',
    );
  }
  return { input, output };
}

function parseDeltaProposalMetadata(
  value: unknown,
  context: string,
): DashboardDeltaProposalMetadata {
  const record = asRecord(value, context);
  const proposal: DashboardDeltaProposalMetadata = {
    proposalType: requireString(record, 'proposal_type', context),
  };
  if (typeof record.description === 'string') proposal.description = record.description;
  if (typeof record.salt === 'string') proposal.salt = record.salt;
  if (record.required_signatures !== undefined)
    proposal.requiredSignatures = requireNonNegativeInteger(
      record.required_signatures,
      'required_signatures',
      context,
    );
  if (typeof record.recipient_id === 'string') proposal.recipientId = record.recipient_id;
  if (typeof record.faucet_id === 'string') proposal.faucetId = record.faucet_id;
  if (typeof record.amount === 'string') proposal.amount = record.amount;
  if (record.note_ids !== undefined)
    proposal.noteIds = assertStringArray(
      requireArray(record, 'note_ids', context),
      'note_ids',
      context,
    );
  if (record.consume_notes_metadata_version !== undefined)
    proposal.consumeNotesMetadataVersion = requireNonNegativeInteger(
      record.consume_notes_metadata_version,
      'consume_notes_metadata_version',
      context,
    );
  if (record.consume_notes_notes !== undefined)
    proposal.consumeNotesNotes = assertStringArray(
      requireArray(record, 'consume_notes_notes', context),
      'consume_notes_notes',
      context,
    );
  if (record.target_threshold !== undefined)
    proposal.targetThreshold = requireNonNegativeInteger(
      record.target_threshold,
      'target_threshold',
      context,
    );
  if (record.signer_commitments !== undefined)
    proposal.signerCommitments = assertStringArray(
      requireArray(record, 'signer_commitments', context),
      'signer_commitments',
      context,
    );
  if (typeof record.new_guardian_pubkey === 'string')
    proposal.newGuardianPubkey = record.new_guardian_pubkey;
  if (typeof record.new_guardian_endpoint === 'string')
    proposal.newGuardianEndpoint = record.new_guardian_endpoint;
  if (typeof record.target_procedure === 'string')
    proposal.targetProcedure = record.target_procedure;
  return proposal;
}

function parseDeltaDetail(value: unknown): DashboardDeltaDetail {
  const ctx = 'delta detail';
  const record = asRecord(value, ctx);
  const detail: DashboardDeltaDetail = {
    accountId: requireString(record, 'account_id', ctx),
    nonce: requireInteger(record, 'nonce', ctx),
    status: parseDeltaStatus(
      requireString(record, 'status', ctx),
      `${ctx}.status`,
    ),
    statusTimestamp: requireString(record, 'status_timestamp', ctx),
    prevCommitment: requireString(record, 'prev_commitment', ctx),
    newCommitment: requireNullableString(record, 'new_commitment', ctx),
    inputNotes: parseDecodedNoteArray(
      requireField(record, 'input_notes', ctx),
      `${ctx}.input_notes`,
    ),
    outputNotes: parseDecodedNoteArray(
      requireField(record, 'output_notes', ctx),
      `${ctx}.output_notes`,
    ),
    vaultChanges: parseVaultChangeArray(
      requireField(record, 'vault_changes', ctx),
      `${ctx}.vault_changes`,
    ),
    storageChanges: parseStorageChangeArray(
      requireField(record, 'storage_changes', ctx),
      `${ctx}.storage_changes`,
    ),
  };
  if (record.retry_count !== undefined) {
    const retry = record.retry_count;
    if (typeof retry !== 'number' || !Number.isInteger(retry) || retry < 0) {
      throw new GuardianOperatorContractError(
        ctx,
        'retry_count must be a non-negative integer when present',
      );
    }
    detail.retryCount = retry;
  }
  if (record.category !== undefined && record.category !== null) {
    detail.category = parseDeltaCategory(
      requireString(record, 'category', ctx),
      `${ctx}.category`,
    );
  }
  if (record.proposal !== undefined && record.proposal !== null) {
    detail.proposal = parseDeltaProposalMetadata(
      record.proposal,
      `${ctx}.proposal`,
    );
  }
  if (Array.isArray(record.decode_warnings) && record.decode_warnings.length > 0) {
    detail.decodeWarnings = (record.decode_warnings as unknown[]).map((w, i) =>
      parseDecodeWarning(w, `${ctx}.decode_warnings[${i}]`),
    );
  }
  if (typeof record.raw_transaction_summary === 'string') {
    detail.rawTransactionSummary = record.raw_transaction_summary;
  }
  return detail;
}

function parseDecodedNoteArray(
  value: unknown,
  context: string,
): DashboardDeltaDecodedNote[] {
  if (!Array.isArray(value)) {
    throw new GuardianOperatorContractError(
      context,
      'expected an array of decoded notes',
    );
  }
  return value.map((entry, i) => parseDecodedNote(entry, `${context}[${i}]`));
}

const NOTE_TAG_VALUES: readonly DashboardDeltaNoteTag[] = [
  'p2id',
  'p2ide',
  'pswap',
  'mint',
  'burn',
  'custom',
];

function parseDecodedNote(
  value: unknown,
  context: string,
): DashboardDeltaDecodedNote {
  const record = asRecord(value, context);
  const tag = requireString(record, 'tag', context);
  if (!(NOTE_TAG_VALUES as readonly string[]).includes(tag)) {
    throw new GuardianOperatorContractError(
      context,
      `unknown note tag "${tag}"`,
    );
  }
  const assets = requireField(record, 'assets', context);
  if (!Array.isArray(assets)) {
    throw new GuardianOperatorContractError(
      context,
      'assets must be an array',
    );
  }
  const note: DashboardDeltaDecodedNote = {
    noteId: requireString(record, 'note_id', context),
    tag: tag as DashboardDeltaNoteTag,
    assets: assets.map((a, i) =>
      parseDecodedAsset(a, `${context}.assets[${i}]`),
    ),
  };
  if (typeof record.sender === 'string') note.sender = record.sender;
  if (typeof record.recipient === 'string') note.recipient = record.recipient;
  return note;
}

function parseDecodedAsset(
  value: unknown,
  context: string,
): DashboardDeltaDecodedAsset {
  const record = asRecord(value, context);
  const kind = requireString(record, 'kind', context);
  if (kind !== 'fungible' && kind !== 'non_fungible') {
    throw new GuardianOperatorContractError(
      context,
      `asset kind must be "fungible" or "non_fungible", got "${kind}"`,
    );
  }
  const asset: DashboardDeltaDecodedAsset = {
    assetId: requireString(record, 'asset_id', context),
    kind: kind as DeltaAssetKind,
  };
  if (typeof record.amount === 'string') asset.amount = record.amount;
  return asset;
}

function parseVaultChangeArray(
  value: unknown,
  context: string,
): DashboardDeltaVaultChange[] {
  if (!Array.isArray(value)) {
    throw new GuardianOperatorContractError(
      context,
      'expected an array of vault changes',
    );
  }
  return value.map((entry, i) => parseVaultChange(entry, `${context}[${i}]`));
}

function parseVaultChange(
  value: unknown,
  context: string,
): DashboardDeltaVaultChange {
  const record = asRecord(value, context);
  const kind = requireString(record, 'kind', context);
  if (kind === 'fungible') {
    return {
      kind: 'fungible',
      assetId: requireString(record, 'asset_id', context),
      change: requireString(record, 'change', context),
    };
  }
  if (kind === 'non_fungible') {
    const added = requireField(record, 'added', context);
    const removed = requireField(record, 'removed', context);
    if (!Array.isArray(added) || !Array.isArray(removed)) {
      throw new GuardianOperatorContractError(
        context,
        'non_fungible added/removed must be arrays',
      );
    }
    return {
      kind: 'non_fungible',
      assetId: requireString(record, 'asset_id', context),
      added: assertStringArray(added as unknown[], 'added', context),
      removed: assertStringArray(removed as unknown[], 'removed', context),
    };
  }
  throw new GuardianOperatorContractError(
    context,
    `vault change kind must be "fungible" or "non_fungible", got "${kind}"`,
  );
}

function parseStorageChangeArray(
  value: unknown,
  context: string,
): DashboardDeltaStorageChange[] {
  if (!Array.isArray(value)) {
    throw new GuardianOperatorContractError(
      context,
      'expected an array of storage changes',
    );
  }
  return value.map((entry, i) => parseStorageChange(entry, `${context}[${i}]`));
}

function parseStorageChange(
  value: unknown,
  context: string,
): DashboardDeltaStorageChange {
  const record = asRecord(value, context);
  const change: DashboardDeltaStorageChange = {
    slotName: requireString(record, 'slot_name', context),
    after: requireNullableString(record, 'after', context),
  };
  if (record.key !== undefined && record.key !== null) {
    change.key = requireString(record, 'key', context);
  }
  if (record.before !== undefined) {
    change.before = requireNullableString(record, 'before', context);
  }
  return change;
}

const DECODE_SECTION_VALUES: readonly DashboardDeltaDecodeSection[] = [
  'tx_summary',
  'metadata',
  'input_notes',
  'output_notes',
  'vault',
  'storage',
];

function parseDecodeWarning(
  value: unknown,
  context: string,
): DashboardDeltaDecodeWarning {
  const record = asRecord(value, context);
  const section = requireString(record, 'section', context);
  if (!(DECODE_SECTION_VALUES as readonly string[]).includes(section)) {
    throw new GuardianOperatorContractError(
      context,
      `unknown decode section "${section}"`,
    );
  }
  return {
    section: section as DashboardDeltaDecodeSection,
    reason: requireString(record, 'reason', context),
  };
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
