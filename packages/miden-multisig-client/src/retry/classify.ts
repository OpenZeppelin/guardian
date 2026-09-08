/**
 * Transient/permanent error classification shared by the prover and node-RPC
 * retry policies, mirroring `is_transient_error_with` in the Rust
 * `guardian-shared` crate: the whole cause chain is walked accumulating
 * evidence, permanent evidence anywhere vetoes transient anywhere, and the
 * transient-wording fallback fires only when no status-level evidence exists
 * in any link. Behavior is pinned by the cross-SDK fixture corpus.
 */

type ErrorRecord = {
  cause?: unknown;
  code?: unknown;
  status?: unknown;
  statusCode?: unknown;
  message?: unknown;
};

type StructuredEvidence = 'transient' | 'permanent' | 'indeterminate';

const TRANSIENT_GRPC = new Set([
  'cancelled',
  'canceled',
  'deadlineexceeded',
  'unavailable',
  'resourceexhausted',
]);

const PERMANENT_GRPC = new Set([
  'invalidargument',
  'failedprecondition',
  'permissiondenied',
  'unauthenticated',
  'notfound',
  'alreadyexists',
  'outofrange',
  'unimplemented',
  'aborted',
  'internal',
  'dataloss',
]);

const TRANSIENT_HTTP = new Set([408, 429, 502, 503, 504]);
const NUMERIC_GRPC_CODES = new Map<number, string>([
  [0, 'ok'],
  [1, 'cancelled'],
  [2, 'unknown'],
  [3, 'invalidargument'],
  [4, 'deadlineexceeded'],
  [5, 'notfound'],
  [6, 'alreadyexists'],
  [7, 'permissiondenied'],
  [8, 'resourceexhausted'],
  [9, 'failedprecondition'],
  [10, 'aborted'],
  [11, 'outofrange'],
  [12, 'unimplemented'],
  [13, 'internal'],
  [14, 'unavailable'],
  [15, 'dataloss'],
  [16, 'unauthenticated'],
]);

const GRPC_MESSAGE_TOKENS: ReadonlyArray<{ token: string; evidence: StructuredEvidence }> = [
  ...[...TRANSIENT_GRPC].map((code) => ({ token: `grpccode${code}`, evidence: 'transient' as const })),
  ...[...PERMANENT_GRPC].map((code) => ({ token: `grpccode${code}`, evidence: 'permanent' as const })),
];

function normalizeCode(value: unknown): string | undefined {
  if (typeof value === 'number') {
    return NUMERIC_GRPC_CODES.get(value);
  }
  return typeof value === 'string' ? value.replaceAll(/[\s_-]/g, '').toLowerCase() : undefined;
}

function grpcEvidence(value: unknown): StructuredEvidence | undefined {
  const code = normalizeCode(value);
  if (code === undefined) {
    return undefined;
  }
  if (TRANSIENT_GRPC.has(code)) {
    return 'transient';
  }
  if (PERMANENT_GRPC.has(code)) {
    return 'permanent';
  }
  if (code === 'unknown' || code === 'ok') {
    return 'indeterminate';
  }
  return undefined;
}

function httpEvidence(value: unknown): StructuredEvidence | undefined {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 400 || value > 599) {
    return undefined;
  }
  return TRANSIENT_HTTP.has(value) ? 'transient' : 'permanent';
}

function httpMessageEvidence(message: string): StructuredEvidence | undefined {
  let hasTransient = false;
  for (const match of message.matchAll(
    /(?:\bhttp(?:\s+status)?|\bstatus)(?:\s+code)?\s*:?\s*(\d{3})\b/g,
  )) {
    const evidence = httpEvidence(Number(match[1]));
    if (evidence === 'permanent') {
      return 'permanent';
    }
    hasTransient ||= evidence === 'transient';
  }
  return hasTransient ? 'transient' : undefined;
}

function grpcMessageEvidence(message: string): StructuredEvidence | undefined {
  const normalized = message.replaceAll(/[^a-z0-9]/g, '');
  let hasTransient = false;
  for (const { token, evidence } of GRPC_MESSAGE_TOKENS) {
    if (normalized.includes(token)) {
      if (evidence === 'permanent') {
        return 'permanent';
      }
      hasTransient ||= evidence === 'transient';
    }
  }
  return hasTransient ? 'transient' : undefined;
}

/**
 * Last-resort transient wording, consulted only when the whole chain carried
 * no status-level evidence. Guarded by the negative classification fixtures
 * (server rejections must never match) — extend with care.
 */
function flattenedTransient(message: string): boolean {
  return [
    'cancelled',
    'canceled',
    'deadline exceeded',
    'timeout',
    'unavailable',
    'resource exhausted',
    'request timeout',
    'too many requests',
    'rate limited',
    'rate limit',
    'bad gateway',
    'service unavailable',
    'gateway timeout',
    'i/o timeout',
    'io timeout',
    'connection reset',
    'broken pipe',
  ].some((signal) => message.includes(signal));
}

function asRecord(value: unknown): ErrorRecord | undefined {
  return typeof value === 'object' && value !== null ? value as ErrorRecord : undefined;
}

/**
 * Walks the error's cause chain and reports whether every piece of evidence
 * points at a transient failure. `extraTransientSignals` extends the
 * transient-wording fallback for one policy without leaking into others,
 * under the same permanent-wins rule.
 */
export function isTransientError(
  error: unknown,
  extraTransientSignals: readonly string[] = [],
): boolean {
  const seen = new Set<object>();
  const messages: string[] = [];
  let hasTransient = false;
  let hasPermanent = false;
  let current: unknown = error;

  const recordMessage = (message: string): void => {
    messages.push(message);
    for (const evidence of [httpMessageEvidence(message), grpcMessageEvidence(message)]) {
      hasTransient ||= evidence === 'transient';
      hasPermanent ||= evidence === 'permanent';
    }
  };

  while (current !== undefined && current !== null) {
    const record = asRecord(current);
    if (record !== undefined) {
      if (seen.has(record)) {
        break;
      }
      seen.add(record);
      for (const evidence of [
        grpcEvidence(record.code),
        httpEvidence(record.status),
        httpEvidence(record.statusCode),
      ]) {
        hasTransient ||= evidence === 'transient';
        hasPermanent ||= evidence === 'permanent';
      }
      recordMessage(
        typeof record.message === 'string'
          ? record.message.toLowerCase()
          : String(current).toLowerCase(),
      );
      current = record.cause;
    } else {
      recordMessage(String(current).toLowerCase());
      break;
    }
  }

  if (hasPermanent) {
    return false;
  }
  if (hasTransient) {
    return true;
  }
  return messages.some(
    (message) =>
      flattenedTransient(message) ||
      extraTransientSignals.some((signal) => message.includes(signal)),
  );
}
