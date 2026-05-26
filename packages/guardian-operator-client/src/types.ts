import type { OperatorPermission } from './permissions.js';

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
  /**
   * Feature 006-operator-authz FR-016 / FR-017: populated only for
   * `insufficient_operator_permission` responses. Lists the
   * permission strings the route required that the authenticated
   * operator does not hold, sorted lexicographically. Absent for
   * every other error code.
   */
  missingPermissions?: readonly string[];
  /**
   * Feature 006-operator-authz FR-016: explicit retryability flag.
   * `false` for permission denials (the contract pins this); absent
   * for every other code so existing parsers see no change.
   */
  retryable?: boolean;
  /**
   * Populated only for `account_paused` responses. RFC 3339 UTC
   * timestamp of the original pause.
   */
  pausedAt?: string;
  /**
   * Populated only for `account_paused`. Reason captured at first
   * pause; `null` is possible when the server has no recorded reason
   * (the request body required a reason, but the field is nullable
   * for forward compatibility).
   */
  pausedReason?: string | null;
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

/**
 * Response shape for `GET /dashboard/session`. `permissions` is sorted
 * lexicographic ASCII and may be empty (means "logged in, no
 * capabilities" — distinct from a 401). The parser validates every
 * entry against the known operator-permission vocabulary, so an
 * unknown string surfaces as a contract error rather than flowing
 * through silently.
 */
export interface SessionInfoResponse {
  operatorId: string;
  permissions: OperatorPermission[];
}

export interface DashboardAccountSummary {
  accountId: string;
  /**
   * Bech32m encoding of the Miden `AccountId` using the network's
   * HRP (`mm` mainnet, `mtst` testnet, `mdev` devnet/local). Absent
   * for EVM accounts and any Miden id that fails to parse.
   */
  accountIdBech32?: string;
  authScheme: string;
  authorizedSignerCount: number;
  hasPendingCandidate: boolean;
  currentCommitment: string | null;
  stateStatus: DashboardAccountStateStatus;
  createdAt: string;
  updatedAt: string;
  /** RFC 3339 UTC timestamp of the original pause; `null` when active. */
  pausedAt: string | null;
  /** Reason captured at first pause; `null` when active. */
  pausedReason: string | null;
}

export interface DashboardAccountDetail extends DashboardAccountSummary {
  authorizedSignerIds: string[];
  stateCreatedAt: string | null;
  stateUpdatedAt: string | null;
}

/** Two-state account lifecycle flag. */
export type AccountStatus = 'active' | 'paused';

export interface PauseAccountResponse {
  accountId: string;
  beforeState: AccountStatus;
  afterState: AccountStatus;
  pausedAt: string;
  pausedReason: string;
}

export interface UnpauseAccountResponse {
  accountId: string;
  beforeState: AccountStatus;
  afterState: AccountStatus;
  reason: string | null;
}

/** Typed details carried on the `GUARDIAN_ACCOUNT_PAUSED` error envelope. */
export interface AccountPausedErrorDetails {
  pausedAt: string;
  pausedReason: string | null;
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
  | 'data_unavailable'
  // Snapshot endpoint distinguishes EVM (permanent) from
  // missing/undecodable state (transient) via separate codes — both
  // must be in the typed union so callers' `isDashboardErrorCode()`
  // narrowing branches on them without falling through.
  | 'unsupported_for_network'
  | 'account_data_unavailable'
  // Feature 006-operator-authz FR-015: the wire string is uppercased
  // per spec to make it visually distinct from the snake_case codes
  // inherited from earlier features. Stable across releases.
  | 'insufficient_operator_permission'
  // Surfaced from mutating endpoints when the target account is
  // paused. Exposed as snake_case to match the rest of the
  // vocabulary; server wire string is `GUARDIAN_ACCOUNT_PAUSED` and
  // the http.ts mapping layer translates between the two.
  | 'account_paused';

export interface PagedResult<T> {
  items: T[];
  nextCursor: string | null;
}

export type DashboardDeltaStatus = 'candidate' | 'canonical' | 'discarded';

/**
 * Closed enumeration of dashboard delta categories. Adding a value
 * here is a wire-contract change that must land on the Rust server first.
 */
export type DashboardDeltaCategory =
  | 'asset_transfer'
  | 'note_consumption'
  | 'note_creation'
  | 'account_storage_change'
  | 'guardian_switch'
  | 'custom';

export type DeltaAssetKind = 'fungible' | 'non_fungible';

export interface DashboardDeltaAssetSummary {
  assetId: string;
  kind: DeltaAssetKind;
  /** Signed decimal magnitude for fungible holdings (e.g. `"-100"`).
   * Omitted for non-fungible holdings; the detail view carries
   * `added` / `removed` lists. */
  amount?: string;
}

export type DeltaCounterpartyDirection = 'in' | 'out';

export interface DashboardDeltaCounterpartySummary {
  accountId: string;
  direction: DeltaCounterpartyDirection;
}

export interface DashboardDeltaNoteCounts {
  input: number;
  output: number;
}

/**
 * Operator-stated proposal intent lifted from the matching
 * `delta_proposals` row at push time. Present only on multisig
 * commits. Mirrors `ProposalMetadataPayload` from the multisig client.
 */
export interface DashboardDeltaProposalMetadata {
  proposalType: string;
  description?: string;
  salt?: string;
  requiredSignatures?: number;
  recipientId?: string;
  faucetId?: string;
  amount?: string;
  noteIds?: string[];
  consumeNotesMetadataVersion?: number;
  consumeNotesNotes?: string[];
  targetThreshold?: number;
  signerCommitments?: string[];
  newGuardianPubkey?: string;
  newGuardianEndpoint?: string;
  targetProcedure?: string;
}

/**
 * Coarse note-tag classification surfaced on the detail endpoint's
 * decoded notes.
 */
export type DashboardDeltaNoteTag =
  | 'p2id'
  | 'p2ide'
  | 'pswap'
  | 'mint'
  | 'burn'
  | 'custom';

export interface DashboardDeltaDecodedAsset {
  assetId: string;
  kind: DeltaAssetKind;
  /** Decimal amount as a string (unsigned; direction is implied by
   * whether this note is an input or output). */
  amount?: string;
}

export interface DashboardDeltaDecodedNote {
  noteId: string;
  tag: DashboardDeltaNoteTag;
  assets: DashboardDeltaDecodedAsset[];
  sender?: string;
  recipient?: string;
}

export type DashboardDeltaVaultChange =
  | {
      kind: 'fungible';
      assetId: string;
      /** Signed decimal magnitude, e.g. `"-100"`, `"+50"`. */
      change: string;
    }
  | {
      kind: 'non_fungible';
      assetId: string;
      added: string[];
      removed: string[];
    };

export interface DashboardDeltaStorageChange {
  /** Human-readable slot name from Miden's `StorageSlotName`
   * (e.g. `"consumed_notes"`). Slots are identified by name, not
   * by numeric index. */
  slotName: string;
  /** Hex `Word` map key for `StorageMap` slot entries; omitted for
   * scalar value slots. For the multisig procedure-threshold overrides
   * map this is the MASM procedure root. */
  key?: string;
  /** Always omitted in v1 — prior slot values are not in the delta. */
  before?: string | null;
  /** Hex `Word` after the change, or `null` when the slot was cleared. */
  after: string | null;
}

export type DashboardDeltaDecodeSection =
  | 'tx_summary'
  | 'metadata'
  | 'input_notes'
  | 'output_notes'
  | 'vault'
  | 'storage';

export interface DashboardDeltaDecodeWarning {
  section: DashboardDeltaDecodeSection;
  reason: string;
}

/**
 * Wire shape for `GET /dashboard/accounts/{account_id}/deltas/{nonce}`.
 *
 * Carries push-time `category` and optional multisig `proposal` at L1
 * plus structured detail sections decoded at request time. Asset,
 * counterparty, and note counts are derivable from the decoded sections.
 */
export interface DashboardDeltaDetail {
  accountId: string;
  /**
   * Per-account monotonic transaction counter. Server type is `u64`,
   * but the JSON wire encoding is a JSON number and the client parses
   * it as a JavaScript `number`. Values above `Number.MAX_SAFE_INTEGER`
   * (2^53 − 1) cannot round-trip without precision loss. Miden account
   * nonces are not expected to reach this range in practice.
   */
  nonce: number;
  status: DashboardDeltaStatus;
  statusTimestamp: string;
  prevCommitment: string;
  newCommitment: string | null;
  retryCount?: number;
  /** Server-curated classification from push-time metadata. */
  category?: DashboardDeltaCategory;
  /** Operator-stated proposal intent for multisig commits. */
  proposal?: DashboardDeltaProposalMetadata;
  inputNotes: DashboardDeltaDecodedNote[];
  outputNotes: DashboardDeltaDecodedNote[];
  vaultChanges: DashboardDeltaVaultChange[];
  storageChanges: DashboardDeltaStorageChange[];
  /** Non-empty when one or more sections could not be decoded. The
   * request still returns 200; affected sections are empty. */
  decodeWarnings?: DashboardDeltaDecodeWarning[];
  /** Present only when the request used `?include=raw`. */
  rawTransactionSummary?: string;
}

export interface DashboardDeltaEntry {
  /** See note on `DashboardDeltaDetail.nonce` for the JS-number precision limit. */
  nonce: number;
  accountId?: string;
  status: DashboardDeltaStatus;
  statusTimestamp: string;
  prevCommitment: string;
  newCommitment: string | null;
  retryCount?: number;

  /** Push-time enrichment spread to L1 on listing endpoints. */
  category?: DashboardDeltaCategory;
  /** Operator intent label (`metadata.proposal.proposal_type` only). */
  proposalType?: string;
  /**
   * Every asset surfaced from the transaction's notes (or, for `p2id`
   * multisig with no decodable summary, the proposal's single declared
   * asset). Multi-asset transactions populate every entry so a one-line
   * row does not show a misleading single-asset summary. Omitted when
   * empty.
   */
  assets?: DashboardDeltaAssetSummary[];
  counterparty?: DashboardDeltaCounterpartySummary;
  noteCounts?: DashboardDeltaNoteCounts;
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
  /** True when the account has a candidate delta in flight that has
   * not yet been canonicalized. The snapshot reflects the current
   * canonical state only — when this is `true`, the vault content
   * may already be stale relative to the chain. UIs SHOULD warn
   * rather than silently display stale data. */
  hasPendingCandidate: boolean;
  vault: DashboardVaultSnapshot;
}

export interface DeltaDetailOptions {
  /** When true, requests `?include=raw` so the server attaches the
   * base64-encoded persisted `TransactionSummary` (debug only). */
  includeRaw?: boolean;
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
