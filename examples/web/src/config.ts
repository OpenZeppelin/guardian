export const GUARDIAN_ENDPOINT = 'http://localhost:3000';
export const MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';
export const MIDEN_DB_NAME = 'MidenClientDB';
export const PROVER_URL = import.meta.env.VITE_PROVER_URL?.trim() || undefined;
const configuredProverAttempts = Number(import.meta.env.VITE_PROVER_MAX_ATTEMPTS ?? 2);
export const PROVER_MAX_ATTEMPTS = Number.isInteger(configuredProverAttempts)
  ? configuredProverAttempts
  : 2;

export const PARA_API_KEY = import.meta.env.VITE_PARA_API_KEY ?? '';
export const PARA_ENVIRONMENT = (import.meta.env.VITE_PARA_ENVIRONMENT ?? 'development') as
  | 'development'
  | 'production';
