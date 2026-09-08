import { isTransientError } from '../retry/classify.js';

/**
 * The node's transport layer renders dropped connections with wording the
 * prover policy deliberately rejects, so these extras apply to node-RPC
 * classification only. Guarded by the negative classification fixtures.
 */
const RPC_TRANSPORT_SIGNALS = ['connection error', 'transport error'];

export function isTransientRpcError(error: unknown): boolean {
  return isTransientError(error, RPC_TRANSPORT_SIGNALS);
}
