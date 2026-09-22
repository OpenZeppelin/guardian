export const GUARDIAN_ENDPOINT = 'http://localhost:3000';
export const MIDEN_RPC_URL = 'https://rpc.devnet.miden.io';
export const MIDEN_DB_NAME = 'MidenClientDB';

/**
 * The chain's fee faucet, bech32 or hex. Required since Miden 0.17: the client
 * builds its protocol configuration from it rather than fetching it from the
 * node, and cannot execute without one; docs/LOCAL_DEV.md#the-fee-faucet says
 * where the value comes from.
 */
export const MIDEN_FEE_FAUCET_ID = requireFeeFaucetId(import.meta.env.VITE_MIDEN_FEE_FAUCET_ID);

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
