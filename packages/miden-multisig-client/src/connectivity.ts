/**
 * Classification for codeless transport failures (feature
 * `009-human-readable-errors`, User Story 3).
 *
 * When Guardian *is* reached, errors arrive as a {@link GuardianHttpError}
 * carrying the server's stable `code` and user-safe `message` — use those.
 * When Guardian is *not* reached (connection refused, DNS, timeout, TLS, or a
 * proxy 5xx with no Guardian body) no error object exists, so the wallet-facing
 * client must classify the failure and supply a friendly connectivity message
 * rather than surfacing raw `"Failed to fetch"` / `"NetworkError"` text.
 *
 * The detection is intentionally string-matching, mirroring the 0xMiden/wallet
 * `connectivity-classify.ts` heuristic: fetch / DOMException / TypeError each
 * surface failures with different shapes, so the message string is the only
 * stable join key.
 */
import { GuardianHttpError } from '@openzeppelin/guardian-client';

/**
 * Connectivity category for a codeless transport failure:
 * - `network` — the browser/OS reports no connectivity at all
 *   (`navigator.onLine === false`); the problem is on the user's side.
 * - `timeout` — the request was sent but timed out or was aborted.
 * - `unreachable` — everything else: Guardian (or an intermediary) could not
 *   be reached or did not answer with a Guardian error object.
 */
export type ConnectivityCategory = 'network' | 'unreachable' | 'timeout';

const CONNECTIVITY_MESSAGE = "Can't reach Guardian right now. Check your connection and try again.";
const GENERIC_MESSAGE = 'Something went wrong. Please try again.';

/**
 * `navigator.onLine` is unreliable as a positive signal, but a `false` means
 * the browser/OS is certain there is no connectivity — categorize as
 * `network` rather than `unreachable` (mirrors the wallet's
 * `isDefinitelyOffline`). Guarded for non-browser (Node) contexts.
 */
function isDefinitelyOffline(): boolean {
  if (typeof navigator === 'undefined') return false;
  if (typeof navigator.onLine !== 'boolean') return false;
  return navigator.onLine === false;
}

/** Best-effort human-readable message from an unknown thrown value. */
export function errorMessage(err: unknown): string {
  return (err as { message?: string } | null | undefined)?.message ?? String(err ?? '');
}

/** Classify a codeless transport failure into a {@link ConnectivityCategory}. */
function classifyTransportError(err: unknown): ConnectivityCategory {
  if (isDefinitelyOffline()) return 'network';
  const lower = errorMessage(err).toLowerCase();
  if (lower.includes('timeout') || lower.includes('timed out') || lower.includes('abort')) {
    return 'timeout';
  }
  return 'unreachable';
}

/**
 * Does this error look like a codeless transport/connectivity failure (vs a
 * semantic Guardian error)? String heuristic — see module docs.
 */
export function isLikelyNetworkError(err: unknown): boolean {
  const lower = errorMessage(err).toLowerCase();
  if (lower.includes('failed to fetch')) return true;
  if (lower.includes('networkerror')) return true;
  if (lower.includes('network error')) return true;
  if (lower.includes('load failed')) return true;
  if (lower.includes('abort')) return true;
  if (lower.includes('timeout') || lower.includes('timed out')) return true;
  if (lower.includes('connection')) return true;
  if (lower.includes('econnrefused') || lower.includes('enotfound')) return true;
  if (lower.includes('dns')) return true;
  return false;
}

/** A normalized, wallet-displayable view of any error thrown by a Guardian call. */
export interface UserFacingError {
  /** Stable Guardian error code, when the server was reached. */
  code?: string;
  /** Connectivity category, when this was a codeless transport failure. */
  category?: ConnectivityCategory;
  /** Short message safe to display verbatim in a wallet UI. */
  userMessage: string;
  /** The original error, for logging/diagnostics. */
  cause: unknown;
}

/**
 * Normalize any error thrown by a Guardian call into a user-facing shape.
 *
 * - A {@link GuardianHttpError} with a parsed `{ code, message }` → the
 *   server's stable code + user-safe message (feature 009).
 * - A reachable host that returned no Guardian error object (e.g. a proxy 5xx
 *   with an HTML body) → treated as a connectivity failure.
 * - A codeless transport rejection (server never reached) → classified and
 *   given the generic connectivity message; the raw transport text is never
 *   surfaced as the primary message.
 */
export function toUserFacingError(err: unknown): UserFacingError {
  if (err instanceof GuardianHttpError) {
    if (err.code && err.userMessage) {
      return { code: err.code, userMessage: err.userMessage, cause: err };
    }
    if (err.status >= 500) {
      return { category: 'unreachable', userMessage: CONNECTIVITY_MESSAGE, cause: err };
    }
    return { userMessage: GENERIC_MESSAGE, cause: err };
  }
  if (isLikelyNetworkError(err)) {
    return { category: classifyTransportError(err), userMessage: CONNECTIVITY_MESSAGE, cause: err };
  }
  return { userMessage: GENERIC_MESSAGE, cause: err };
}
