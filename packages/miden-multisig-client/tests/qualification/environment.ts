import { grpcMessageEvidence, httpMessageEvidence } from '../../src/retry/classify.js';

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
 * Status evidence is read the way `isTransientError` reads it: permanent
 * anywhere vetoes transient anywhere. Its generic wording fallback
 * (`unavailable`, `timeout`, `cancelled`, ...) is not used. A reason is the
 * driver's own sentence wrapped around whatever it caught, and those words turn
 * up in both halves, so a GUARDIAN 500 behind `delta history unavailable` or a
 * server message about a quorum timeout would otherwise stop blocking the
 * nightly. Only `ENVIRONMENT_SIGNALS`, each specific to a failing link, stands
 * in for a missing status. The Rust driver applies the same rule, and both are
 * pinned to `fixtures/qualification/environment-classification.json`.
 */

/**
 * Wording that stands in for a missing status: the node-RPC transport signals,
 * the operating-system and runtime error names that reach the driver as bare
 * text through the WASM boundary, where the typed cause is already lost,
 * tonic's rendering of the transient gRPC codes, and the link-specific part of
 * the retry fallback.
 *
 * Every entry must be unambiguous evidence of a link failure. Guardian's own
 * error codes and messages travel in these same strings, so wording a scenario
 * could assert on (`network_error`, for one) or a bare `timeout` or
 * `unavailable` stays out deliberately.
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
  'the service is currently unavailable',
  'the operation was cancelled',
  'the deadline expired before the operation could complete',
  'deadline exceeded',
  'i/o timeout',
  'connection reset',
  'broken pipe',
  'bad gateway',
  'gateway timeout',
  'service unavailable',
];

/** Whether a failure reason is the environment failing under the suite. */
export function isEnvironmental(reason: string): boolean {
  const message = reason.toLowerCase();
  const evidence = [httpMessageEvidence(message), grpcMessageEvidence(message)];
  if (evidence.includes('permanent')) return false;
  if (evidence.includes('transient')) return true;
  return ENVIRONMENT_SIGNALS.some((signal) => message.includes(signal));
}
