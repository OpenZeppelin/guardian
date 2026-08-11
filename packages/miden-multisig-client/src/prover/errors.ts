import { isTransientError } from '../retry/classify.js';

/**
 * Prover-policy classification: the shared classifier with no transport-text
 * extras — a bare "connection error" from a prover is treated as its
 * considered answer.
 */
export function isTransientProverError(error: unknown): boolean {
  return isTransientError(error);
}
