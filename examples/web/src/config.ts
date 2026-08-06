export const GUARDIAN_ENDPOINT = 'http://localhost:3000';
export const MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';
export const MIDEN_DB_NAME = 'MidenClientDB';
export const PROVER_URL = import.meta.env.VITE_PROVER_URL?.trim() || undefined;

function parseMaxAttempts(raw: string | undefined, min: number, fallback: number): number {
  const value = Number(raw?.trim() || fallback);
  const valid = Number.isInteger(value) && value >= min && value <= 4_294_967_295;
  return valid ? value : fallback;
}

export const PROVER_MAX_ATTEMPTS = parseMaxAttempts(
  import.meta.env.VITE_PROVER_MAX_ATTEMPTS,
  0,
  2,
);
export const RPC_MAX_ATTEMPTS = parseMaxAttempts(import.meta.env.VITE_RPC_MAX_ATTEMPTS, 1, 2);

export const PARA_API_KEY = import.meta.env.VITE_PARA_API_KEY ?? '';
export const PARA_ENVIRONMENT = (import.meta.env.VITE_PARA_ENVIRONMENT ?? 'development') as
  | 'development'
  | 'production';
