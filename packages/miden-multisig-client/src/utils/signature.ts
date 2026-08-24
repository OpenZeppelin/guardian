import { AdviceMap, Felt, FeltArray, Poseidon2, Signature, Word } from '@miden-sdk/miden-sdk';
import * as midenSdk from '@miden-sdk/miden-sdk';
import { EcdsaFormat } from './ecdsa.js';
import { hexToBytes, normalizeHexWord } from './encoding.js';
import type { ProposalSignatureEntry, SignatureScheme } from '../types.js';

export const ECDSA_AUTH_SCHEME_ID = 1;
export const FALCON_AUTH_SCHEME_ID = 2;

export function authSchemeId(scheme: SignatureScheme): number {
  return scheme === 'ecdsa' ? ECDSA_AUTH_SCHEME_ID : FALCON_AUTH_SCHEME_ID;
}

export function signatureHexToBytes(
  hex: string,
  scheme: SignatureScheme = 'falcon',
): Uint8Array {
  const sigBytes = hexToBytes(hex);
  const withPrefix = new Uint8Array(sigBytes.length + 1);
  withPrefix[0] = authSchemeId(scheme);
  withPrefix.set(sigBytes, 1);
  return withPrefix;
}

/**
 * `toPreparedSignature` is the SDK binding for the Rust
 * `Signature::to_encoded_signature`, so both Falcon and ECDSA advice payloads
 * come from upstream rather than being packed here. For ECDSA it emits
 * `QX[8] || QY[8] || SIG_R[8] || SIG_S[8]` and recovers the public key from the
 * message, which is why the signature must carry its recovery byte.
 */
export function buildSignatureAdviceEntry(
  pubkeyCommitment: Word,
  message: Word,
  signature: Signature,
): { key: Word; values: Felt[] } {
  const elements = new FeltArray([
    ...pubkeyCommitment.toFelts(),
    ...message.toFelts(),
  ]);
  const key = Poseidon2.hashElements(elements);

  return { key, values: signature.toPreparedSignature(message) };
}

/**
 * `toPreparedSignature` recovers the ECDSA public key from the message, and an
 * unrecoverable signature aborts inside WASM with `unreachable`, which poisons
 * the module instance for the rest of the session. Recover first through the
 * pure-JS path so a malformed cosigner or GUARDIAN signature surfaces as a
 * normal error instead.
 */
export function assertEcdsaSignatureRecoverable(
  signatureHex: string,
  messageHex: string,
  expectedPublicKeyHex: string,
): void {
  let recovered: string;
  try {
    recovered = EcdsaFormat.recoverCompressedPublicKeyHex(
      hexToBytes(messageHex),
      hexToBytes(signatureHex),
    );
  } catch (error) {
    throw new Error(`ECDSA signature does not recover a public key: ${String(error)}`);
  }

  const expected = EcdsaFormat.compressPublicKey(expectedPublicKeyHex);
  if (recovered.toLowerCase() !== expected.toLowerCase()) {
    throw new Error(
      `ECDSA signature recovers public key ${recovered}, which does not match the expected ${expected}`,
    );
  }
}

export function tryComputeEcdsaCommitmentHex(pubkeyHex: string): string | null {
  return tryComputeCommitmentHex(pubkeyHex, 'ecdsa');
}

export function tryComputeCommitmentHex(
  pubkeyHex: string,
  scheme: SignatureScheme,
): string | null {
  const bytes = hexToBytes(pubkeyHex);
  const withPrefix = new Uint8Array(bytes.length + 1);
  withPrefix[0] = authSchemeId(scheme);
  withPrefix.set(bytes, 1);

  try {
    const { PublicKey } = midenSdk as any;
    const instance = PublicKey.deserialize(withPrefix);
    return normalizeHexWord(instance.toCommitment().toHex());
  } catch {
    return null;
  }
}

export function mergeSignatureAdviceMaps(
  advice: AdviceMap,
  entries: Array<{ key: Word; values: Felt[] }>,
): AdviceMap {
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
): ProposalSignatureEntry {
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
