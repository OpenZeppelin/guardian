import { AdviceMap, Felt, FeltArray, Rpo256, Signature, Word } from '@demox-labs/miden-sdk';
import { hexToBytes, normalizeHexWord } from './encoding.js';

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

