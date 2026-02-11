export const PSM_ENDPOINT = 'https://psm-stg.openzeppelin.com';
export const MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';
export const MIDEN_DB_NAME = 'MidenClientDB';

export const PARA_API_KEY = import.meta.env.VITE_PARA_API_KEY ?? '';
export const PARA_ENVIRONMENT = (import.meta.env.VITE_PARA_ENVIRONMENT ?? 'development') as
  | 'development'
  | 'production';
