export type DashboardAccountStateStatus = 'available' | 'unavailable';

export interface GuardianOperatorHttpErrorData {
  success: false;
  /**
   * Stable, machine-readable error code emitted by the server. Clients
   * SHOULD branch on this rather than on `error` (the human message) or
   * the HTTP status alone. Codes added by feature
   * `005-operator-dashboard-metrics` are typed via {@link DashboardErrorCode};
   * other codes (e.g. `account_not_found`, `authentication_failed`) are
   * forwarded as raw strings.
   */
  code?: string;
  error: string;
  retryAfterSecs?: number;
}

export interface GuardianOperatorHttpClientOptions {
  baseUrl: string;
  fetch?: typeof fetch;
  credentials?: RequestCredentials;
  headers?: HeadersInit;
}

export interface OperatorChallenge {
  domain: string;
  commitment: string;
  nonce: string;
  expiresAt: string;
  signingDigest: string;
}

export interface OperatorChallengeResponse {
  success: true;
  challenge: OperatorChallenge;
}

export interface VerifyOperatorRequest {
  commitment: string;
  signature: string;
}

export interface VerifyOperatorResponse {
  success: true;
  operatorId: string;
  expiresAt: string;
}

export interface LogoutOperatorResponse {
  success: true;
}

export interface DashboardAccountSummary {
  accountId: string;
  authScheme: string;
  authorizedSignerCount: number;
  hasPendingCandidate: boolean;
  currentCommitment: string | null;
  stateStatus: DashboardAccountStateStatus;
  createdAt: string;
  updatedAt: string;
}

export interface DashboardAccountDetail extends DashboardAccountSummary {
  authorizedSignerIds: string[];
  stateCreatedAt: string | null;
  stateUpdatedAt: string | null;
}

/**
 * @deprecated Removed in feature `005-operator-dashboard-metrics`. The
 * account list endpoint now returns
 * `PagedResult<DashboardAccountSummary>` (see
 * `GuardianOperatorHttpClient.listAccounts`). Aggregate inventory
 * totals are exposed via `getDashboardInfo()`.
 */
export type DashboardAccountsResponse = never;

export interface DashboardAccountResponse {
  success: true;
  account: DashboardAccountDetail;
}

// ---------------------------------------------------------------------------
// Pagination, info, and history types introduced by feature
// `005-operator-dashboard-metrics`.
//
// Most type bodies are filled in by later phases:
//   - `PagedResult<T>` and `DashboardErrorCode` are populated by T010.
//   - `DashboardInfoResponse` is populated by T023 (US2).
//   - `DashboardDeltaEntry` is populated by T030 (US3).
//   - `DashboardProposalEntry` is populated by T038 (US4).
//
// Phase 1 (T003) declares them so subsequent phases can extend without
// new exports / re-exports. Each starts as a structural placeholder; the
// final shapes match `contracts/dashboard.openapi.yaml`.
// ---------------------------------------------------------------------------

/**
 * Stable error codes that the dashboard endpoints can emit. The server's
 * 401 path uses `authentication_failed` (the cookie/session middleware
 * variant); a hypothetical token-bearer path could emit `unauthorized`,
 * but that does not happen on the operator dashboard surface today.
 *
 * Mirrors the `code` enum on `ErrorBody` in
 * `005-operator-dashboard-metrics/contracts/dashboard.openapi.yaml`.
 */
export type DashboardErrorCode =
  | 'authentication_failed'
  | 'account_not_found'
  | 'invalid_cursor'
  | 'invalid_limit'
  | 'invalid_status_filter'
  | 'data_unavailable';

export interface PagedResult<T> {
  items: T[];
  nextCursor: string | null;
}

export type DashboardDeltaStatus = 'candidate' | 'canonical' | 'discarded';

export interface DashboardDeltaEntry {
  nonce: number;
  accountId?: string;
  status: DashboardDeltaStatus;
  statusTimestamp: string;
  prevCommitment: string;
  newCommitment: string | null;
  retryCount?: number;
}

export interface DashboardProposalEntry {
  commitment: string;
  nonce: number;
  accountId?: string;
  proposerId: string;
  originatingTimestamp: string;
  signaturesCollected: number;
  signaturesRequired: number;
  prevCommitment: string;
  newCommitment: string | null;
}

/**
 * Global delta feed entry. Identical to {@link DashboardDeltaEntry}
 * but `accountId` is required (every entry on the global feed is
 * tagged with the account it belongs to).
 */
export interface DashboardGlobalDeltaEntry extends DashboardDeltaEntry {
  accountId: string;
}

/**
 * Global proposal feed entry. Identical to
 * {@link DashboardProposalEntry} but `accountId` is required.
 */
export interface DashboardGlobalProposalEntry extends DashboardProposalEntry {
  accountId: string;
}

/**
 * Optional `?status=` filter on the global delta feed (FR-033).
 * Accepts either a single value or an array; the wrapper serializes
 * to comma-separated.
 */
export type DashboardGlobalDeltaStatusFilter =
  | DashboardDeltaStatus
  | DashboardDeltaStatus[];

export interface GlobalDeltasOptions {
  limit?: number;
  cursor?: string;
  status?: DashboardGlobalDeltaStatusFilter;
}

export interface DashboardInfoResponse {
  serviceStatus: 'healthy' | 'degraded';
  environment: string;
  totalAccountCount: number;
  latestActivity: string | null;
  deltaStatusCounts: {
    candidate: number;
    canonical: number;
    discarded: number;
  };
  inFlightProposalCount: number;
  degradedAggregates: string[];
}
