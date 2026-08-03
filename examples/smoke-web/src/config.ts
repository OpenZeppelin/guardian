export const DEFAULT_GUARDIAN_ENDPOINT = 'http://localhost:3000';
export const DEFAULT_MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';
export const DEFAULT_MIDEN_DB_NAME = 'MidenClientDB';
export const DEFAULT_BROWSER_LABEL = '';
export const DEFAULT_APP_NAME = 'Miden Multisig Smoke';
export const DEFAULT_PROVER_URL = import.meta.env.VITE_PROVER_URL?.trim() || undefined;
const configuredProverAttempts = Number(
  import.meta.env.VITE_PROVER_MAX_ATTEMPTS?.trim() || 2,
);
const hasValidProverAttempts =
  Number.isInteger(configuredProverAttempts) &&
  configuredProverAttempts >= 0 &&
  configuredProverAttempts <= 4_294_967_295;
export const DEFAULT_PROVER_MAX_ATTEMPTS = hasValidProverAttempts
  ? configuredProverAttempts
  : 2;

export const PARA_API_KEY = import.meta.env.VITE_PARA_API_KEY ?? '';
export const PARA_ENVIRONMENT = (import.meta.env.VITE_PARA_ENVIRONMENT ?? 'development') as
  | 'development'
  | 'production';
