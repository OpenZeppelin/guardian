export interface RpcRetryPolicy {
  maxAttempts?: number;
}

export interface RpcConfig {
  retry?: RpcRetryPolicy;
}

export interface ResolvedRpcConfig {
  readonly maxAttempts: number;
}

const DEFAULT_MAX_ATTEMPTS = 2;
const MAX_U32 = 4_294_967_295;

function normalizeMaxAttempts(value: number | undefined): number {
  if (value === undefined) {
    return DEFAULT_MAX_ATTEMPTS;
  }
  if (!Number.isFinite(value) || !Number.isInteger(value) || value < 0 || value > MAX_U32) {
    throw new Error('rpc.retry.maxAttempts must be an integer between 0 and 4294967295');
  }
  return Math.max(1, value);
}

export function resolveRpcConfig(config: RpcConfig | undefined): ResolvedRpcConfig {
  return {
    maxAttempts: normalizeMaxAttempts(config?.retry?.maxAttempts),
  };
}
