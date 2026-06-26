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

/** Connectivity category for a codeless transport failure. */
export type ConnectivityCategory = 'network' | 'unreachable' | 'timeout';

const CONNECTIVITY_MESSAGE = "Can't reach Guardian right now. Check your connection and try again.";
const GENERIC_MESSAGE = 'Something went wrong. Please try again.';

/**
 * Does this error look like a codeless transport/connectivity failure (vs a
 * semantic Guardian error)? String heuristic — see module docs.
 */
export function isLikelyNetworkError(err: unknown): boolean {
  const message = (err as { message?: string } | null | undefined)?.message ?? String(err ?? '');
  const lower = message.toLowerCase();
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
    return { category: 'unreachable', userMessage: CONNECTIVITY_MESSAGE, cause: err };
  }
  return { userMessage: GENERIC_MESSAGE, cause: err };
}
