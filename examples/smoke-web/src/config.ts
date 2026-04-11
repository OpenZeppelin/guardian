export const DEFAULT_GUARDIAN_ENDPOINT = 'http://localhost:3000';
export const DEFAULT_MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';
export const DEFAULT_MIDEN_DB_NAME = 'MidenClientDB';
export const DEFAULT_BROWSER_LABEL = '';
export const DEFAULT_APP_NAME = 'Miden Multisig Smoke';

export const PARA_API_KEY = import.meta.env.VITE_PARA_API_KEY ?? '';
export const PARA_ENVIRONMENT = (import.meta.env.VITE_PARA_ENVIRONMENT ?? 'development') as
  | 'development'
  | 'production';
