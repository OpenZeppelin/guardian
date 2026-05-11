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

/**
 * `GET /dashboard/accounts/{id}` returns the account detail directly
 * (no `success`, no `account` wrapper). The endpoint relies on the
 * HTTP status code for success/failure and on the `DashboardErrorCode`
 * body for typed errors.
 *
 * Kept as a named alias so callers and library code can still reference
 * "the response shape" without having to spell out the detail type at
 * each site. New shapes added to this endpoint MUST keep the bare
 * payload form.
 */
export type DashboardAccountResponse = DashboardAccountDetail;

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
  /**
   * Multisig proposal type tag carried in
   * `delta_payload.metadata.proposal_type` on the underlying record
   * (e.g. `"add_signer"`, `"p2id"`, `"change_threshold"`, ...). Absent
   * for direct `push_delta` single-key Miden writes and for EVM
   * deltas, which carry no metadata blob.
   */
  proposalType?: string;
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
  /** See {@link DashboardDeltaEntry.proposalType}. In practice always
   * populated for in-flight multisig proposals on this endpoint. */
  proposalType?: string;
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

/** Fungible asset entry in an account vault snapshot. `amount` is a
 * string to keep `u64` precision safe across JS. Decimal handling and
 * USD conversion are dashboard-client concerns; Guardian does not
 * carry token metadata or price oracles. */
export interface DashboardVaultFungibleEntry {
  faucetId: string;
  amount: string;
}

/** Non-fungible asset entry. `vaultKey` is the canonical Miden
 * identifier for the asset within the vault. */
export interface DashboardVaultNonFungibleEntry {
  faucetId: string;
  vaultKey: string;
}

export interface DashboardVaultSnapshot {
  fungible: DashboardVaultFungibleEntry[];
  nonFungible: DashboardVaultNonFungibleEntry[];
}

/**
 * Decoded snapshot of one account's stored state at the commitment
 * Guardian last canonicalized. Source of truth is Guardian's stored
 * state — no live Miden RPC calls. New fields land here as additive
 * top-level keys derivable from the existing state blob.
 */
export interface DashboardAccountSnapshot {
  /** Hex state commitment the snapshot was decoded from. Equals
   * `DashboardAccountDetail.currentCommitment` for the same account
   * at the same point in time; callers can correlate the snapshot
   * with a delta history entry by matching on this hex. */
  commitment: string;
  /** RFC3339 wall-clock time of the underlying state row's
   * `updated_at` — i.e. when Guardian last persisted the
   * canonicalized state this snapshot was decoded from. Equals
   * `DashboardAccountDetail.stateUpdatedAt` for the same account at
   * the same point in time. */
  updatedAt: string;
  vault: DashboardVaultSnapshot;
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

/** Build identity for the running `guardian-server` binary. */
export interface DashboardBuildInfo {
  /** `guardian-server` package version (`CARGO_PKG_VERSION`). */
  version: string;
  /** Short git SHA at build time, or `"unknown"` when unavailable. */
  gitCommit: string;
  /** Cargo build profile: `"debug"` or `"release"`. */
  profile: 'debug' | 'release';
  /** RFC3339 wall-clock time the server initialized dashboard state. */
  startedAt: string;
}

/** Canonicalization worker config when the server runs in
 * candidate-commit mode. `null` indicates optimistic-commit mode. */
export interface DashboardCanonicalizationConfig {
  checkIntervalSeconds: number;
  maxRetries: number;
  submissionGracePeriodSeconds: number;
}

/** Backend configuration snapshot. */
export interface DashboardBackendInfo {
  /** `"filesystem"` or `"postgres"` based on the server's compiled
   * feature flag. */
  storage: 'filesystem' | 'postgres';
  /** Acknowledgement signature schemes wired into the server's
   * `AckRegistry`. */
  supportedAckSchemes: string[];
  /** `null` in optimistic-commit mode. */
  canonicalization: DashboardCanonicalizationConfig | null;
}

export interface DashboardInfoResponse {
  serviceStatus: 'healthy' | 'degraded';
  environment: string;
  build: DashboardBuildInfo;
  backend: DashboardBackendInfo;
  totalAccountCount: number;
  /** Counts of accounts grouped by stable auth-method label
   * (`"miden_falcon"`, `"miden_ecdsa"`, `"evm"`). Empty when marked
   * degraded — check `degradedAggregates` for
   * `"accounts_by_auth_method"`. */
  accountsByAuthMethod: Record<string, number>;
  latestActivity: string | null;
  deltaStatusCounts: {
    candidate: number;
    canonical: number;
    discarded: number;
  };
  inFlightProposalCount: number;
  degradedAggregates: string[];
}
