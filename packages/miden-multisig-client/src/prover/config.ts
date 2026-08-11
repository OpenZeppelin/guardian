import { TransactionProver } from '@miden-sdk/miden-sdk';

export interface ProverRetryPolicy {
  maxAttempts?: number;
}

export interface ProverConfig {
  url?: string;
  retry?: ProverRetryPolicy;
}

export interface ResolvedProverConfig {
  readonly kind: 'injected' | 'remote';
  readonly url?: string;
  readonly maxAttempts: number;
  createProver(): TransactionProver | undefined;
}

const DEFAULT_MAX_ATTEMPTS = 2;
const MAX_U32 = 4_294_967_295;

function normalizeMaxAttempts(value: number | undefined): number {
  if (value === undefined) {
    return DEFAULT_MAX_ATTEMPTS;
  }
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < 0 || value > MAX_U32) {
    throw new Error('prover.retry.maxAttempts must be an integer between 0 and 4294967295');
  }
  return Math.max(1, value);
}

function normalizeUrl(value: string): string {
  const trimmed = value.trim();
  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new Error('prover.url must be an absolute HTTP(S) URL with a host');
  }
  if (!['http:', 'https:'].includes(parsed.protocol) || parsed.hostname === '') {
    throw new Error('prover.url must be an absolute HTTP(S) URL with a host');
  }
  return parsed.href;
}

export function resolveProverConfig(
  config: ProverConfig | undefined,
  defaultProver: TransactionProver | null,
): ResolvedProverConfig {
  const maxAttempts = normalizeMaxAttempts(config?.retry?.maxAttempts);

  if (config?.url !== undefined) {
    const url = normalizeUrl(config.url);
    return {
      kind: 'remote',
      url,
      maxAttempts,
      createProver: () => TransactionProver.newRemoteProver(url),
    };
  }

  const endpoint = defaultProver?.endpoint();
  if (defaultProver === null || endpoint === undefined) {
    return {
      kind: 'injected',
      maxAttempts: 1,
      createProver: () => undefined,
    };
  }

  const descriptor = defaultProver.serialize();
  return {
    kind: 'remote',
    url: endpoint,
    maxAttempts,
    createProver: () => TransactionProver.deserialize(descriptor),
  };
}
