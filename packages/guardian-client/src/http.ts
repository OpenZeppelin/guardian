import {
  type GuardianErrorCode,
  normalizeGuardianErrorCode,
} from './error-codes.js';
import type {
  AbandonCandidateResponse,
  AbandonStatus,
  ConfigureRequest,
  ConfigureResponse,
  CreateSessionRequest,
  CreateSessionResponse,
  DeltaObject,
  DeltaProposalRequest,
  DeltaProposalResponse,
  ExecutionDelta,
  HistoryOptions,
  HistoryPage,
  LookupResponse,
  PubkeyResponse,
  PushDeltaResponse,
  SessionEndReason,
  SessionRequestSigner,
  SignProposalRequest,
  SignatureScheme,
  Signer,
  StateObject,
  StatusResponse,
  CanonicalNonce,
} from './types.js';
import { RequestAuthPayload } from './auth-request.js';
import { fromServerExecution, fromServerExecutionCapability } from './execution.js';
import type {
  ProposalExecution,
  ServerCurrentExecution,
  ServerProposalExecution,
} from './execution.js';
import type {
  ServerAbandonCandidateRequest,
  ServerAbandonCandidateResponse,
  ServerCanonicalNonceResponse,
  ServerDeltaObject,
  ServerDeltaProposalResponse,
  ServerHistoryPage,
  ServerLookupResponse,
  ServerProposalsResponse,
  ServerPubkeyResponse,
  ServerStateObject,
  ServerConfigureResponse,
  ServerCreateSessionResponse,
  ServerPushDeltaResponse,
  ServerRevokeAllSessionsRequest,
  ServerRevokeAllSessionsResponse,
  ServerRevokeSessionResponse,
  ServerStatusResponse,
} from './server-types.js';
import {
  fromServerCanonicalNonce,
  fromServerConfigureResponse,
  fromServerCreateSessionResponse,
  fromServerDeltaObject,
  fromServerHistoryPage,
  fromServerLookupResponse,
  fromServerStateObject,
  toServerConfigureRequest,
  toServerCreateSessionRequest,
  toServerDeltaProposalRequest,
  toServerExecutionDelta,
  toServerSignProposalRequest,
} from './conversion.js';

/**
 * Structured machine-readable side-data on a GUARDIAN error
 * (feature `009-human-readable-errors`). `retryable` is always present.
 */
export interface GuardianErrorMeta {
  retryable: boolean;
  retryAfterSecs?: number;
  missingPermissions?: string[];
  pausedAt?: string;
  pausedReason?: string | null;
  releasedAt?: string;
  /**
   * Signature scheme the server refused for a new account registration.
   * Present only when `code === 'signature_scheme_not_allowed'`.
   */
  scheme?: string;
  /**
   * Schemes this Guardian accepts for new accounts
   * (`GUARDIAN_ALLOWED_ACCOUNT_SCHEMES`). Present only when
   * `code === 'signature_scheme_not_allowed'`.
   */
  allowedSchemes?: string[];
  /**
   * The proposal whose execution holds the account. Present only when
   * `code === 'execution_conflict'`.
   */
  blockingProposalId?: string;
  /**
   * The proposal type GUARDIAN refuses to execute because the client that finishes its
   * follow-up work must execute it. Present only when `code === 'proposal_executes_locally'`.
   */
  proposalType?: string;
  /**
   * Why GUARDIAN leaves the proposal to local execution. Present only when
   * `code === 'proposal_executes_locally'`: `switch_guardian` for a guardian switch.
   */
  reason?: LocalExecutionReason;
}

/** Why GUARDIAN refuses to execute a proposal and leaves it to local execution. */
export type LocalExecutionReason = 'switch_guardian';

function localExecutionReason(value: unknown): LocalExecutionReason | undefined {
  switch (value) {
    case 'switch_guardian':
      return value;
    default:
      return undefined;
  }
}

interface ParsedGuardianError {
  /** Typed code, or `null` when the wire code is outside the known vocabulary. */
  code: GuardianErrorCode | null;
  /** Verbatim wire code, kept even when it does not narrow to the union. */
  rawCode: string;
  message: string;
  meta: GuardianErrorMeta;
}

/**
 * Parse a GUARDIAN error body `{ code, message, meta }` (feature 009),
 * mapping the server's snake_case `meta` fields to camelCase. Returns
 * `undefined` for non-JSON or non-conforming bodies — including a missing
 * `meta` or a non-boolean `meta.retryable`, which the contract requires;
 * treating those as conforming would silently misclassify retryability for
 * bodies from older servers or intermediary proxies.
 */
function parseGuardianErrorBody(body: string): ParsedGuardianError | undefined {
  let json: unknown;
  try {
    json = JSON.parse(body);
  } catch {
    return undefined;
  }
  if (typeof json !== 'object' || json === null) return undefined;
  const obj = json as Record<string, unknown>;
  if (typeof obj.code !== 'string' || typeof obj.message !== 'string') return undefined;

  if (typeof obj.meta !== 'object' || obj.meta === null || Array.isArray(obj.meta)) {
    return undefined;
  }
  const rawMeta = obj.meta as Record<string, unknown>;
  if (typeof rawMeta.retryable !== 'boolean') return undefined;
  const code = normalizeGuardianErrorCode(obj.code);
  const meta: GuardianErrorMeta = { retryable: rawMeta.retryable };
  if (
    typeof rawMeta.retry_after_secs === 'number' &&
    Number.isInteger(rawMeta.retry_after_secs) &&
    rawMeta.retry_after_secs >= 0
  ) {
    meta.retryAfterSecs = rawMeta.retry_after_secs;
  }
  if (Array.isArray(rawMeta.missing_permissions)) {
    meta.missingPermissions = rawMeta.missing_permissions.filter(
      (x): x is string => typeof x === 'string'
    );
  }
  if (typeof rawMeta.paused_at === 'string') meta.pausedAt = rawMeta.paused_at;
  if (typeof rawMeta.released_at === 'string') meta.releasedAt = rawMeta.released_at;
  if (typeof rawMeta.scheme === 'string') meta.scheme = rawMeta.scheme;
  if (
    Array.isArray(rawMeta.allowed_schemes) &&
    rawMeta.allowed_schemes.every((x): x is string => typeof x === 'string')
  ) {
    meta.allowedSchemes = rawMeta.allowed_schemes;
  }
  if (typeof rawMeta.blocking_proposal_id === 'string') {
    meta.blockingProposalId = rawMeta.blocking_proposal_id;
  }
  if (typeof rawMeta.proposal_type === 'string') meta.proposalType = rawMeta.proposal_type;
  const reason = localExecutionReason(rawMeta.reason);
  if (reason !== undefined) meta.reason = reason;
  if (typeof rawMeta.paused_reason === 'string' || rawMeta.paused_reason === null) {
    meta.pausedReason = rawMeta.paused_reason as string | null;
  }
  return { code, rawCode: obj.code, message: obj.message, meta };
}

/**
 * Error thrown by the GUARDIAN HTTP client. Parses the `{ code, message, meta }`
 * error body (feature 009): branch on {@link code}, display {@link userMessage}.
 */
export class GuardianHttpError extends Error {
  /**
   * Typed, compiler-checked Guardian error code (issue #318), normalized to
   * snake_case (e.g. `account_paused`, `account_released`,
   * `commitment_mismatch`). `null` when the body is not a conforming JSON
   * envelope OR the server emitted a code outside this client's known
   * vocabulary — in the latter case {@link rawCode} still carries the
   * verbatim wire string. Branch on this rather than on `body` text or the
   * HTTP status alone; comparing against a non-member literal is a type
   * error, so typos are caught at compile time.
   */
  public readonly code: GuardianErrorCode | null;
  /**
   * Verbatim wire code as the server sent it (e.g.
   * `GUARDIAN_ACCOUNT_PAUSED`), including codes a newer server may emit
   * that this client does not know. `null` only when the body carried no
   * conforming envelope. For logging/telemetry; branch on {@link code}.
   */
  public readonly rawCode: string | null;
  /** Short, user-safe message — safe to display verbatim in a wallet UI. */
  readonly userMessage?: string;
  /** Structured side-data (`retryable`, `retryAfterSecs`, …). */
  readonly meta?: GuardianErrorMeta;
  /**
   * RFC 3339 UTC timestamp at which the guardian released the account
   * after it switched to a different guardian. Convenience accessor for
   * `meta.releasedAt`; present only when `code === 'account_released'`
   * (wire form `GUARDIAN_ACCOUNT_RELEASED`, HTTP 409); the account is
   * terminal on this server until re-onboarded via `configure`.
   */
  public readonly releasedAt: string | null;

  private readonly headerRetryAfterSecs?: number;

  constructor(
    public readonly status: number,
    public readonly statusText: string,
    public readonly body: string,
    retryAfterHeader?: string | null
  ) {
    // Only the parsed, user-safe message is folded into Error.message; the
    // raw body (which may carry backend/proxy internals) stays on the `body`
    // field for diagnostics only.
    const parsed = parseGuardianErrorBody(body);
    super(`GUARDIAN HTTP error ${status}: ${statusText}${parsed ? ` - ${parsed.message}` : ''}`);
    this.name = 'GuardianHttpError';
    this.code = parsed?.code ?? null;
    this.rawCode = parsed?.rawCode ?? null;
    this.userMessage = parsed?.message;
    this.meta = parsed?.meta;
    this.releasedAt = parsed?.meta.releasedAt ?? null;
    this.headerRetryAfterSecs = parseRetryAfterSeconds(retryAfterHeader);
  }

  /**
   * Whether the server marked this error safe to retry: `meta.retryable`
   * from the envelope, falling back to the status class (429 rejections
   * happen before any handler runs). Mirrors the Rust client's
   * `ClientError::is_retryable`.
   */
  isRetryable(): boolean {
    return this.meta?.retryable ?? this.status === 429;
  }

  /**
   * Server-provided backoff hint in seconds: the `Retry-After` header,
   * falling back to `meta.retryAfterSecs`. Unparseable values mean no
   * hint. Mirrors the Rust client's `ClientError::retry_after`.
   */
  retryAfterSecs(): number | undefined {
    return this.headerRetryAfterSecs ?? this.meta?.retryAfterSecs;
  }
}

// Decimal digits only, mirroring the Rust client's `parse::<u64>()`: no
// signs, exponents, hex, or empty strings (`Number('')` is 0).
function parseRetryAfterSeconds(header: string | null | undefined): number | undefined {
  if (typeof header !== 'string') return undefined;
  const trimmed = header.trim();
  if (!/^\d+$/.test(trimmed)) return undefined;
  const secs = Number(trimmed);
  return Number.isSafeInteger(secs) ? secs : undefined;
}

/**
 * A session is set aside this long before it expires, so a request never
 * reaches the server already expired; requests then fall back to the wallet.
 */
const SESSION_EXPIRY_MARGIN_MS = 30_000;

/** Codes that end a session-signed request's session for good. */
const SESSION_END_REASONS: Record<string, SessionEndReason> = {
  session_expired: 'expired',
  session_revoked: 'revoked',
  authentication_failed: 'rejected',
};

/**
 * Which key signs a per-account request. Mirrors the server: requests are
 * wallet-signed unless the route is session-eligible.
 */
type AuthMode = 'session' | 'wallet';

function authHeaders(
  publicKey: string,
  signature: string,
  timestamp: number,
  format?: 'eip712' | 'session'
): Record<string, string> {
  return {
    'x-pubkey': publicKey,
    'x-signature': signature,
    'x-timestamp': timestamp.toString(),
    ...(format ? { 'x-auth-format': format } : {}),
  };
}

/**
 * Minimal HTTP client for GUARDIAN server.
 */
export class GuardianHttpClient {
  private signer: Signer | null = null;
  private session: SessionRequestSigner | null = null;
  private readonly endedSessions = new WeakSet<SessionRequestSigner>();
  private readonly baseUrl: string;
  private lastTimestamp = 0;

  constructor(baseUrl: string) {
    this.baseUrl = baseUrl;
  }

  /**
   * Monotonic timestamp for auth headers. Strictly increasing across calls
   * within a single client instance so concurrent or rapid-fire requests
   * never produce duplicate `x-timestamp` values.
   */
  private nextTimestamp(): number {
    const now = Date.now();
    const ts = now > this.lastTimestamp ? now : this.lastTimestamp + 1;
    this.lastTimestamp = ts;
    return ts;
  }

  setSigner(signer: Signer): void {
    this.signer = signer;
  }

  /**
   * Sign session-eligible requests (reads, proposal list/get, proposal
   * create/sign, `pushDelta`) with a registered delegated signer instead of
   * the wallet. Every other route, including `configure`,
   * `abandonCandidate`, account lookup and `revokeAllSessions`, uses the
   * wallet signer. Pass `null` to stop using the session.
   */
  setSession(session: SessionRequestSigner | null): void {
    this.session = session;
  }

  /**
   * The session in use, or `null` when none is set, no wallet signer is set,
   * the session was granted by another wallet than the current signer, or it
   * is about to expire. A session reaching its expiry margin is dropped and
   * told so.
   */
  getSession(): SessionRequestSigner | null {
    const session = this.session;
    if (!session) {
      return null;
    }
    if (session.expiresAt * 1000 - SESSION_EXPIRY_MARGIN_MS <= Date.now()) {
      this.endSession(session, 'expired');
      return null;
    }
    if (
      !this.signer ||
      this.signer.commitment.toLowerCase() !== session.signerCommitment.toLowerCase()
    ) {
      return null;
    }
    return session;
  }

  /**
   * Stops using `session` if it is still the current one, and tells it why
   * once. A newer session set meanwhile stays in use.
   */
  private endSession(session: SessionRequestSigner, reason: SessionEndReason): void {
    if (this.session === session) {
      this.session = null;
    }
    if (!this.endedSessions.has(session)) {
      this.endedSessions.add(session);
      session.onEnded?.(reason);
    }
  }

  /** Register a wallet-signed session grant (`POST /session`). */
  async createSession(request: CreateSessionRequest): Promise<CreateSessionResponse> {
    const response = await this.fetch('/session', {
      method: 'POST',
      body: JSON.stringify(toServerCreateSessionRequest(request)),
    });
    return fromServerCreateSessionResponse((await response.json()) as ServerCreateSessionResponse);
  }

  /**
   * Revoke a session on the server (`POST /session/logout`), the current one
   * by default, and stop using it. Returns whether the server still had it
   * active; `false` when there is none. On error the session stays in use.
   */
  async revokeSession(session: SessionRequestSigner | null = this.session): Promise<boolean> {
    if (!session) {
      return false;
    }
    const timestamp = this.nextTimestamp();
    const signature = await session.signLogout(timestamp);
    const response = await this.fetch('/session/logout', {
      method: 'POST',
      headers: authHeaders(session.publicKey, signature, timestamp),
    });
    const data = (await response.json()) as ServerRevokeSessionResponse;
    this.endSession(session, 'logout');
    return data.revoked;
  }

  /**
   * Revoke every session of the wallet signer on the server
   * (`POST /session/revoke-all`), including sessions started elsewhere, and
   * stop using the session that was current when the call was made. A
   * session started while the call is in flight was issued after it and
   * stays in use. Signed by the wallet. Returns how many sessions the server
   * revoked.
   */
  async revokeAllSessions(): Promise<number> {
    const signer = this.signer;
    if (!signer) {
      throw new Error('No signer configured. Call setSigner() first.');
    }
    if (!signer.signSessionRevokeAll) {
      throw new Error('This signer cannot sign Guardian session revocations');
    }
    const revoking = this.session;
    const timestamp = this.nextTimestamp();
    const signature = await signer.signSessionRevokeAll(signer.commitment, timestamp);
    const body: ServerRevokeAllSessionsRequest = { signer_commitment: signer.commitment };
    const response = await this.fetch('/session/revoke-all', {
      method: 'POST',
      headers: authHeaders(signer.publicKey, signature, timestamp, signer.requestAuthFormat),
      body: JSON.stringify(body),
    });
    const data = (await response.json()) as ServerRevokeAllSessionsResponse;
    if (revoking?.signerCommitment.toLowerCase() === signer.commitment.toLowerCase()) {
      this.endSession(revoking, 'logout');
    }
    return data.revoked;
  }

  /**
   * Guardian's ACK commitment for one signature scheme (`GET /pubkey`).
   * Omitting `scheme` returns the Falcon key even on a server that admits only
   * ECDSA accounts, so pass the scheme of the account the commitment is for.
   */
  async getPubkey(scheme?: SignatureScheme): Promise<PubkeyResponse> {
    const query = scheme ? `?scheme=${scheme}` : '';
    const response = await this.fetch(`/pubkey${query}`, { method: 'GET' });
    const data = (await response.json()) as ServerPubkeyResponse;
    return {
      commitment: data.commitment,
      pubkey: data.pubkey,
    };
  }

  async getStatus(): Promise<StatusResponse> {
    const response = await this.fetch('/status', { method: 'GET' });
    const data = (await response.json()) as ServerStatusResponse;
    return {
      status: data.status,
      version: data.version,
      gitCommit: data.git_commit,
      environment: data.environment,
      startedAt: data.started_at,
      uptimeSeconds: data.uptime_seconds,
      ...(data.execution === undefined || data.execution === null
        ? {}
        : { execution: fromServerExecutionCapability(data.execution) }),
      ...(data.sessions
        ? {
            sessions: {
              maxTtlSeconds: data.sessions.max_ttl_seconds,
            },
          }
        : {}),
    };
  }

  async configure(request: ConfigureRequest): Promise<ConfigureResponse> {
    const serverRequest = toServerConfigureRequest(request);
    // Configuration changes who the account trusts: always the wallet.
    const response = await this.fetchAuthenticated('/configure', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, request.accountId, serverRequest);
    const server = (await response.json()) as ServerConfigureResponse;
    return fromServerConfigureResponse(server);
  }

  async getState(accountId: string): Promise<StateObject> {
    const requestQuery = { account_id: accountId };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/state?${params}`, {
      method: 'GET',
    }, accountId, requestQuery, 'session');
    const server = (await response.json()) as ServerStateObject;
    return fromServerStateObject(server);
  }

  /**
   * Nonce and commitment of the latest canonical state, without the state
   * blob (`GET /state/nonce`). A client can skip `getState` when the
   * returned nonce is below its local account nonce, or equal to it with
   * the same commitment; an equal nonce at a different commitment means the
   * local account diverged from GUARDIAN, so it fetches the state.
   */
  async getCanonicalNonce(accountId: string): Promise<CanonicalNonce> {
    const requestQuery = { account_id: accountId };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/state/nonce?${params}`, {
      method: 'GET',
    }, accountId, requestQuery, 'session');
    const server = (await response.json()) as ServerCanonicalNonceResponse;
    return fromServerCanonicalNonce(server);
  }

  /**
   * Resolve a public-key commitment to the set of account IDs whose
   * authorization set contains it. Authentication is by proof-of-possession:
   * the configured signer MUST hold the private key behind `keyCommitmentHex`
   * and implement `signLookupMessage`. Returns an empty list when the
   * commitment is not authorized for any account.
   */
  async lookupAccountByKeyCommitment(keyCommitmentHex: string): Promise<LookupResponse> {
    const params = new URLSearchParams({ key_commitment: keyCommitmentHex });
    const response = await this.fetchLookupAuthenticated(
      `/state/lookup?${params}`,
      { method: 'GET' },
      keyCommitmentHex
    );
    return fromServerLookupResponse((await response.json()) as ServerLookupResponse);
  }

  async getDeltaProposals(accountId: string): Promise<DeltaObject[]> {
    const requestQuery = { account_id: accountId };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/proposal?${params}`, {
      method: 'GET',
    }, accountId, requestQuery, 'session');
    const data = (await response.json()) as ServerProposalsResponse;
    return data.proposals.map(fromServerDeltaObject);
  }

  async getDeltaProposal(accountId: string, commitment: string): Promise<DeltaObject> {
    const requestQuery = { account_id: accountId, commitment };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/proposal/single?${params}`, {
      method: 'GET',
    }, accountId, requestQuery, 'session');
    const data = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(data);
  }

  async pushDeltaProposal(request: DeltaProposalRequest): Promise<DeltaProposalResponse> {
    const serverRequest = toServerDeltaProposalRequest(request);
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, request.accountId, serverRequest, 'session');
    const server = (await response.json()) as ServerDeltaProposalResponse;
    return {
      delta: fromServerDeltaObject(server.delta),
      commitment: server.commitment,
    };
  }

  /**
   * Ask Guardian to prove and submit a threshold-met proposal. Resolves once the request is
   * accepted; poll {@link getDeltaProposalExecution} for the outcome. Repeating the request while
   * the execution runs returns it with `newlyAccepted: false`.
   *
   * Signed by the session while one is set, over the payload tagged as `ExecuteDeltaProposal`:
   * the body names the same fields as a status read, and the tag keeps a signed read from
   * authenticating an execution request.
   */
  async executeDeltaProposal(accountId: string, proposalId: string): Promise<ProposalExecution> {
    const serverRequest = {
      account_id: accountId,
      proposal_id: proposalId,
    };
    const response = await this.fetchSigned('/delta/proposal/execution', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, accountId, RequestAuthPayload.forOperation('ExecuteDeltaProposal', serverRequest), 'session');
    return fromServerExecution((await response.json()) as ServerProposalExecution);
  }

  /** The latest execution attempt of a proposal. */
  async getDeltaProposalExecution(accountId: string, proposalId: string): Promise<ProposalExecution> {
    const requestQuery = { account_id: accountId, proposal_id: proposalId };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/proposal/execution?${params}`, {
      method: 'GET',
    }, accountId, requestQuery, 'session');
    return fromServerExecution((await response.json()) as ServerProposalExecution);
  }

  /** The account's in-flight execution, or `null`. A finished execution is not in flight. */
  async getCurrentExecution(accountId: string): Promise<ProposalExecution | null> {
    const requestQuery = { account_id: accountId };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/execution/current?${params}`, {
      method: 'GET',
    }, accountId, requestQuery, 'session');
    const server = (await response.json()) as ServerCurrentExecution;
    return server.execution === null ? null : fromServerExecution(server.execution);
  }

  /**
   * Request abandonment of a pending canonicalization candidate whose
   * transaction will never land on-chain (issue #319).
   *
   * Records an abandon *intent* (`202 Accepted`): the delta stays a
   * candidate — the account stays locked — until the guardian's worker
   * confirms over the abandon quarantine that the transaction did not
   * land, then discards the delta as `client_abandoned` and releases the
   * account (typically well under a minute). Poll {@link abandonStatus}
   * for the resolution. Refused with `GUARDIAN_CANDIDATE_LANDED` (409)
   * when the transaction actually landed. Retries are idempotent and
   * preserve the original request timestamp.
   *
   * `proposalId` names the transaction: its summary's commitment, which
   * is the proposal id of a multisig proposal. A Guardian execution can
   * archive the delta and take its nonce; the abandon then still answers
   * for this transaction.
   */
  async abandonCandidate(
    accountId: string,
    nonce: number,
    proposalId: string,
  ): Promise<AbandonCandidateResponse> {
    const serverRequest: ServerAbandonCandidateRequest = {
      account_id: accountId,
      nonce,
      proposal_id: proposalId,
    };
    // Abandoning changes the account's canonical chain: always the wallet.
    const response = await this.fetchAuthenticated('/delta/candidate/abandon', {
      method: 'POST',
      body: JSON.stringify(serverRequest),
    }, accountId, serverRequest);
    const server = (await response.json()) as ServerAbandonCandidateResponse;
    return {
      accountId: server.account_id,
      nonce: server.nonce,
      state: server.state,
      abandonRequestedAt: server.abandon_requested_at,
      commitment: server.commitment,
    };
  }

  /**
   * Poll the resolution of an abandon request made with
   * {@link abandonCandidate}: `'waiting'` while the quarantine runs,
   * `'landed'` if the transaction landed after all (the delta
   * canonicalized), `'abandoned'` once the delta is discarded as
   * client-abandoned and the account released, `'retained'` when the
   * guardian stopped verifying and released the account but the
   * on-chain outcome is still uncertain (reconciliation may promote the
   * delta until its retention TTL expires — sync and check chain before
   * replacing it), `'unexpected'` for any state no abandon flow
   * produces (including a missing delta).
   *
   * `commitment` is the one {@link abandonCandidate} returned. Another
   * delta can take the nonce (a Guardian execution archives the abandoned
   * one): once it is canonical the abandoned transaction can no longer
   * land (`'abandoned'`), and while it is a candidate the nonce is still
   * contested (`'waiting'`).
   */
  async abandonStatus(accountId: string, nonce: number, commitment: string): Promise<AbandonStatus> {
    let delta: DeltaObject;
    try {
      delta = await this.getDelta(accountId, nonce);
    } catch (e) {
      if (e instanceof GuardianHttpError && e.code === 'delta_not_found') {
        return 'unexpected';
      }
      throw e;
    }
    if (delta.newCommitment?.toLowerCase() !== commitment.toLowerCase()) {
      switch (delta.status.status) {
        case 'canonical':
          return 'abandoned';
        case 'candidate':
          return 'waiting';
        default:
          return 'unexpected';
      }
    }
    switch (delta.status.status) {
      case 'candidate':
        return 'waiting';
      case 'canonical':
        return 'landed';
      // The Guardian gave up verifying and released the account (issue
      // #345): unlocked, but unresolved — deliberately distinct from
      // 'abandoned', which would wrongly imply the transaction did not
      // land.
      case 'retained':
        return 'retained';
      case 'discarded':
        return delta.status.reason === 'client_abandoned' ? 'abandoned' : 'unexpected';
      default:
        return 'unexpected';
    }
  }


  async signDeltaProposal(request: SignProposalRequest): Promise<DeltaObject> {
    const serverRequest = toServerSignProposalRequest(request);
    const response = await this.fetchAuthenticated('/delta/proposal', {
      method: 'PUT',
      body: JSON.stringify(serverRequest),
    }, request.accountId, serverRequest, 'session');
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  /**
   * Push a delta for Guardian's acknowledgement. Signed by the session while
   * one is set: Guardian acknowledges a multisig delta only once its
   * proposal carries enough verified cosigner approvals, which the wallet
   * signs. A Guardian that takes this route only from the wallet (an older
   * release, or an account without a multisig state) answers
   * `wallet_signature_required`, and the wallet signs the push again; the
   * session stays in use for every other route.
   */
  async pushDelta(delta: ExecutionDelta): Promise<PushDeltaResponse> {
    const serverDelta = toServerExecutionDelta(delta);
    const init = { method: 'POST', body: JSON.stringify(serverDelta) };
    const response = await this.fetchAuthenticated(
      '/delta',
      init,
      delta.accountId,
      serverDelta,
      'session'
    ).catch((err: unknown) => {
      if (err instanceof GuardianHttpError && err.code === 'wallet_signature_required') {
        return this.fetchAuthenticated('/delta', init, delta.accountId, serverDelta, 'wallet');
      }
      throw err;
    });
    const server = (await response.json()) as ServerPushDeltaResponse;
    return {
      accountId: server.account_id,
      nonce: server.nonce,
      newCommitment: server.new_commitment,
      ackSig: server.ack_sig,
      ackPubkey: server.ack_pubkey,
      ackScheme: server.ack_scheme,
    };
  }

  async getDelta(accountId: string, nonce: number): Promise<DeltaObject> {
    const requestPayload = {
      account_id: accountId,
      nonce,
    };
    const requestQuery = {
      account_id: accountId,
      nonce: nonce.toString(),
    };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta?${params}`, {
      method: 'GET',
    }, accountId, requestPayload, 'session');
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  async getDeltaSince(accountId: string, fromNonce: number): Promise<DeltaObject> {
    const requestPayload = {
      account_id: accountId,
      nonce: fromNonce,
    };
    const requestQuery = {
      account_id: accountId,
      nonce: fromNonce.toString(),
    };
    const params = new URLSearchParams(requestQuery);
    const response = await this.fetchAuthenticated(`/delta/since?${params}`, {
      method: 'GET',
    }, accountId, requestPayload, 'session');
    const server = (await response.json()) as ServerDeltaObject;
    return fromServerDeltaObject(server);
  }

  /**
   * Fetch one page of the account's canonical delta history
   * (issue #413), newest-first by nonce, with decoded input/output
   * note summaries. Pass `options.cursor` from a previous page's
   * `nextCursor` to resume; an absent `nextCursor` on the result means
   * the feed is exhausted. Read-only: served while the account is
   * paused. Only transactions pushed through Guardian appear.
   */
  async getDeltaHistory(accountId: string, options: HistoryOptions = {}): Promise<HistoryPage> {
    // Signed payload and query string must carry the same values: the
    // server signs the canonical JSON of its query struct, where limit
    // stays a string and omitted parameters are omitted keys.
    const requestPayload: Record<string, string> = { account_id: accountId };
    if (options.limit !== undefined) {
      requestPayload.limit = options.limit.toString();
    }
    if (options.cursor !== undefined) {
      requestPayload.cursor = options.cursor;
    }
    const params = new URLSearchParams(requestPayload);
    const response = await this.fetchAuthenticated(`/delta/history?${params}`, {
      method: 'GET',
    }, accountId, requestPayload, 'session');
    const server = (await response.json()) as ServerHistoryPage;
    return fromServerHistoryPage(server);
  }

  private async fetch(path: string, init: RequestInit): Promise<Response> {
    const url = `${this.baseUrl}${path}`;
    const response = await fetch(url, {
      ...init,
      headers: {
        'Content-Type': 'application/json',
        ...init.headers,
      },
    });

    if (!response.ok) {
      const body = await response.text();
      throw new GuardianHttpError(
        response.status,
        response.statusText,
        body,
        response.headers.get('Retry-After')
      );
    }

    return response;
  }

  /**
   * Authenticated fetch for the lookup endpoint. Cannot reuse
   * `fetchAuthenticated`, which builds an `AuthRequestPayload` bound to an
   * `accountId` (the value lookup is trying to discover). Digest construction
   * is delegated to the signer's `signLookupMessage`.
   */
  private async fetchLookupAuthenticated(
    path: string,
    init: RequestInit,
    keyCommitmentHex: string
  ): Promise<Response> {
    if (!this.signer) {
      throw new Error('No signer configured. Call setSigner() first.');
    }
    if (!this.signer.signLookupMessage) {
      throw new Error(
        'Signer does not implement signLookupMessage. Account recovery by key requires a ' +
          'signer that signs the lookup hash in raw or EIP-712 format; the canonical ' +
          'lookup-hash helper lives in @openzeppelin/miden-multisig-client.'
      );
    }

    const timestamp = this.nextTimestamp();
    const signature = await this.signer.signLookupMessage(keyCommitmentHex, timestamp);

    return this.fetch(path, {
      ...init,
      headers: {
        ...init.headers,
        ...authHeaders(this.signer.publicKey, signature, timestamp, this.signer.requestAuthFormat),
      },
    });
  }

  private async fetchAuthenticated(
    path: string,
    init: RequestInit,
    accountId: string,
    requestPayload: unknown,
    mode: AuthMode = 'wallet'
  ): Promise<Response> {
    return this.fetchSigned(path, init, accountId, RequestAuthPayload.fromRequest(requestPayload), mode);
  }

  private async fetchSigned(
    path: string,
    init: RequestInit,
    accountId: string,
    authPayload: RequestAuthPayload,
    mode: AuthMode,
    retries = 2
  ): Promise<Response> {
    const timestamp = this.nextTimestamp();
    const session = mode === 'session' ? this.getSession() : null;
    const headers = session
      ? authHeaders(
          session.publicKey,
          await session.signRequest(accountId, timestamp, authPayload),
          timestamp,
          'session'
        )
      : await this.walletAuthHeaders(accountId, timestamp, authPayload);

    try {
      return await this.fetch(path, {
        ...init,
        headers: {
          ...init.headers,
          ...headers,
        },
      });
    } catch (err) {
      // A session Guardian ended or no longer accepts (unknown key after a
      // restart, rotated Guardian key or network) is dead for good: stop
      // using it so the caller can start a new one; meanwhile the wallet
      // signs again.
      if (session && err instanceof GuardianHttpError) {
        const reason = SESSION_END_REASONS[err.code ?? ''];
        if (reason) {
          this.endSession(session, reason);
        }
      }
      // Replay rejections are transient: the request was correctly signed
      // but lost the server's per-signer timestamp CAS. Retry with a fresh
      // timestamp and signature, branching only on the dedicated
      // `authentication_replay` code (issue #367); terminal authentication
      // failures (invalid signature, clock outside the skew window) are
      // never retried.
      if (
        retries > 0 &&
        err instanceof GuardianHttpError &&
        err.code === 'authentication_replay'
      ) {
        await new Promise((resolve) => setTimeout(resolve, 50));
        return this.fetchSigned(path, init, accountId, authPayload, mode, retries - 1);
      }
      throw err;
    }
  }

  private async walletAuthHeaders(
    accountId: string,
    timestamp: number,
    authPayload: RequestAuthPayload
  ): Promise<Record<string, string>> {
    if (!this.signer) {
      throw new Error('No signer configured. Call setSigner() first.');
    }
    const signature = this.signer.signRequest
      ? await this.signer.signRequest(accountId, timestamp, authPayload)
      : await this.signer.signAccountIdWithTimestamp(accountId, timestamp);
    return authHeaders(this.signer.publicKey, signature, timestamp, this.signer.requestAuthFormat);
  }
}
