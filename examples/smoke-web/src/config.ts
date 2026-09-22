export const DEFAULT_GUARDIAN_ENDPOINT = 'http://localhost:3000';
export const DEFAULT_MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';
export const DEFAULT_MIDEN_DB_NAME = 'MidenClientDB';

/**
 * The chain's fee faucet, bech32 or hex. Required since Miden 0.17: the client
 * builds its protocol configuration from it and cannot execute without one. The
 * node does not serve it over RPC yet, so it comes from the environment.
 */
export const DEFAULT_MIDEN_FEE_FAUCET_ID = requireFeeFaucetId(import.meta.env.VITE_MIDEN_FEE_FAUCET_ID);

function requireFeeFaucetId(raw: string | undefined): string {
  const value = raw?.trim();
  if (!value) {
    throw new Error(
      'VITE_MIDEN_FEE_FAUCET_ID is not set: name the chain\'s fee faucet (bech32 or hex) so the ' +
        'Miden client can build its protocol configuration',
    );
  }
  return value;
}
export const DEFAULT_BROWSER_LABEL = '';
export const DEFAULT_APP_NAME = 'Miden Multisig Smoke';
export const DEFAULT_PROVER_URL = import.meta.env.VITE_PROVER_URL?.trim() || undefined;

function parseMaxAttempts(raw: string | undefined, min: number, fallback: number): number {
  const value = Number(raw?.trim() || fallback);
  const valid = Number.isInteger(value) && value >= min && value <= 4_294_967_295;
  return valid ? value : fallback;
}

export const DEFAULT_PROVER_MAX_ATTEMPTS = parseMaxAttempts(
  import.meta.env.VITE_PROVER_MAX_ATTEMPTS,
  0,
  2,
);
export const DEFAULT_RPC_MAX_ATTEMPTS = parseMaxAttempts(
  import.meta.env.VITE_RPC_MAX_ATTEMPTS,
  1,
  2,
);
