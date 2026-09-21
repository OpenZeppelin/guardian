import { isTransientError } from '../../src/retry/classify.js';

/**
 * Tells the network the suite runs over apart from the product it tests.
 *
 * The live profile drives a public Miden network and a remote prover. Neither
 * is under this repository's control, and both fail in ways that look exactly
 * like a scenario failing: a dropped connection mid-execution, a prover
 * deadline, a node that stops answering. Reporting those as product defects is
 * how a nightly schedule stops being read, so a failure whose evidence points
 * at the link is reported as environment-blocked instead.
 *
 * The rule is `isTransientError` unchanged, which is itself the mirror of the
 * Rust `guardian-shared` classifier. The Rust driver applies the same rule, and
 * both are pinned to `fixtures/qualification/environment-classification.json`.
 */

/**
 * Transport wording the shared fallback does not carry: the node-RPC transport
 * signals plus the operating-system and runtime error names that reach the
 * driver as bare text through the WASM boundary, where the typed cause is
 * already lost.
 *
 * Every entry must be unambiguous evidence of a link failure. Guardian's own
 * error codes travel in these same strings, so wording a scenario could assert
 * on (`network_error`, for one) stays out deliberately.
 */
export const ENVIRONMENT_SIGNALS: readonly string[] = [
  'connection error',
  'transport error',
  'timed out',
  'etimedout',
  'econnreset',
  'econnrefused',
  'econnaborted',
  'ehostunreach',
  'enetunreach',
  'epipe',
  'eai_again',
  'socket hang up',
  'fetch failed',
  'err_http2_stream_error',
  'invalid content type: application/grpc',
];

/** Whether a failure reason is the environment failing under the suite. */
export function isEnvironmental(reason: string): boolean {
  return isTransientError(reason, ENVIRONMENT_SIGNALS);
}
