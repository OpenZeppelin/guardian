import { AdviceMap, Felt, FeltArray, Rpo256, Signature, Word } from '@miden-sdk/miden-sdk';
import { hexToBytes, normalizeHexWord } from './encoding.js';
import type { ProposalSignatureEntry } from '../types.js';

export function signatureHexToBytes(hex: string): Uint8Array {
  const sigBytes = hexToBytes(hex);
  const withPrefix = new Uint8Array(sigBytes.length + 1);
  withPrefix[0] = 0;
  withPrefix.set(sigBytes, 1);
  return withPrefix;
}

export function buildSignatureAdviceEntry(
  pubkeyCommitment: Word,
  message: Word,
  signature: Signature,
): { key: Word; values: Felt[] } {
  const elements = new FeltArray([
    ...pubkeyCommitment.toFelts(),
    ...message.toFelts(),
  ]);
  const key = Rpo256.hashElements(elements);
  const values = signature.toPreparedSignature(message);
  return { key, values };
}

export function mergeSignatureAdviceMaps(advice: AdviceMap, entries: Array<{ key: Word; values: Felt[] }>): AdviceMap {
  for (const entry of entries) {
    advice.insert(entry.key, new FeltArray(entry.values));
  }
  return advice;
}

export function toWord(hex: string): Word {
  return Word.fromHex(normalizeHexWord(hex));
}

export function normalizeSignerCommitment(signerId: string): string {
  const hex = signerId.startsWith('0x') || signerId.startsWith('0X')
    ? signerId.slice(2)
    : signerId;

  if (hex.length !== 64 || !/^[0-9a-fA-F]+$/.test(hex)) {
    throw new Error(`expected signerId as 32-byte hex, got ${signerId}`);
  }

  return normalizeHexWord(signerId);
}

export function canonicalizeSignature(
  signature: ProposalSignatureEntry,
  signerCommitments: Set<string>,
) : ProposalSignatureEntry {
  try {
    const signerId = normalizeSignerCommitment(signature.signerId);
    if (!signerCommitments.has(signerId)) {
      throw new Error(`signer ${signerId} is not part of this multisig`);
    }

    return {
      ...signature,
      signerId,
    };
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(message);
  }
}
