export const GUARDIAN_ENDPOINT = 'http://localhost:3000';
export const MIDEN_RPC_URL = 'https://rpc.testnet.miden.io';
export const MIDEN_DB_NAME = 'MidenClientDB';

export const PARA_API_KEY = import.meta.env.VITE_PARA_API_KEY ?? '';
export const PARA_ENVIRONMENT = (import.meta.env.VITE_PARA_ENVIRONMENT ?? 'development') as
  | 'development'
  | 'production';
