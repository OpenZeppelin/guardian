import { AdviceMap, Felt, FeltArray, Rpo256, Signature, Word } from '@demox-labs/miden-sdk';
import * as midenSdk from '@demox-labs/miden-sdk';
import { hexToBytes, normalizeHexWord } from './encoding.js';
import type { SignatureScheme } from '../types.js';

export function signatureHexToBytes(hex: string, scheme: SignatureScheme = 'falcon'): Uint8Array {
  const sigBytes = hexToBytes(hex);
  const withPrefix = new Uint8Array(sigBytes.length + 1);
  withPrefix[0] = scheme === 'ecdsa' ? 1 : 0;
  withPrefix.set(sigBytes, 1);
  return withPrefix;
}

function bytesToPackedU32Felts(bytes: Uint8Array): Felt[] {
  const felts: Felt[] = [];
  for (let i = 0; i < bytes.length; i += 4) {
    let packed = 0;
    for (let j = 0; j < 4 && i + j < bytes.length; j++) {
      packed |= bytes[i + j] << (j * 8);
    }
    felts.push(new Felt(BigInt(packed >>> 0)));
  }
  return felts;
}

function encodeEcdsaSignatureFelts(pubkeyBytes: Uint8Array, sigBytes: Uint8Array): Felt[] {
  const pkFelts = bytesToPackedU32Felts(pubkeyBytes);
  const sigFelts = bytesToPackedU32Felts(sigBytes);
  const encoded = [...pkFelts, ...sigFelts];
  encoded.reverse();
  return encoded;
}

export function buildSignatureAdviceEntry(
  pubkeyCommitment: Word,
  message: Word,
  signature: Signature,
  ecdsaPubkeyHex?: string,
  ecdsaSigHex?: string,
): { key: Word; values: Felt[] } {
  const elements = new FeltArray([
    ...pubkeyCommitment.toFelts(),
    ...message.toFelts(),
  ]);
  const key = Rpo256.hashElements(elements);

  let values: Felt[];
  if (ecdsaPubkeyHex && ecdsaSigHex) {
    const pkBytes = hexToBytes(ecdsaPubkeyHex);
    const sigBytes = hexToBytes(ecdsaSigHex);
    values = encodeEcdsaSignatureFelts(pkBytes, sigBytes);
  } else {
    values = signature.toPreparedSignature(message);
  }

  return { key, values };
}

export function tryComputeEcdsaCommitmentHex(pubkeyHex: string): string | null {
  const bytes = hexToBytes(pubkeyHex);
  const withPrefix = new Uint8Array(bytes.length + 1);
  withPrefix[0] = 1;
  withPrefix.set(bytes, 1);

  try {
    const { PublicKey } = midenSdk as any;
    const instance = PublicKey.deserialize(withPrefix);
    return normalizeHexWord(instance.toCommitment().toHex());
  } catch {
    return null;
  }
}

export function verifyEcdsaCommitment(
  pubkeyHex: string,
  expectedCommitmentHex: string,
): { match: boolean; computedHex: string; packedFelts: string[]; error?: string } {
  try {
    const bytes = hexToBytes(pubkeyHex);
    const packedU32Values: number[] = [];
    for (let i = 0; i < bytes.length; i += 4) {
      let packed = 0;
      for (let j = 0; j < 4 && i + j < bytes.length; j++) {
        packed |= bytes[i + j] << (j * 8);
      }
      packedU32Values.push(packed >>> 0);
    }

    const packedFelts = bytesToPackedU32Felts(bytes);
    const feltArray = new FeltArray(packedFelts);
    const computed = Rpo256.hashElements(feltArray);
    const computedHex = normalizeHexWord(computed.toHex());
    const expectedNorm = normalizeHexWord(expectedCommitmentHex);
    return {
      match: computedHex === expectedNorm,
      computedHex,
      packedFelts: packedU32Values.map(v => v.toString()),
    };
  } catch (e) {
    return {
      match: false,
      computedHex: `ERROR: ${e}`,
      packedFelts: [],
      error: String(e),
    };
  }
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
