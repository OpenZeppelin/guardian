import { AdviceMap, Felt, FeltArray, Poseidon2, Signature, Word } from '@miden-sdk/miden-sdk';
import * as midenSdk from '@miden-sdk/miden-sdk';
import { EcdsaFormat } from './ecdsa.js';
import { midenTransactionTypedData, typedDataDigest } from './eip712.js';
import { hexToBytes, normalizeHexWord } from './encoding.js';
import { wordToBytes } from './word.js';
import { secp256k1 } from '@noble/curves/secp256k1';
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

// Little-endian ASCII "EIP712", matching the protocol advice-key domain.
const EIP712_SIGNATURE_KEY_DOMAIN = 0x323137504945n;

function littleEndianU32Limbs(bytes: Uint8Array): Felt[] {
  const limbs: Felt[] = [];
  for (let i = 28; i >= 0; i -= 4) {
    const limb = (bytes[i] << 24) | (bytes[i + 1] << 16) | (bytes[i + 2] << 8) | bytes[i + 3];
    limbs.push(new Felt(BigInt(limb >>> 0)));
  }
  return limbs;
}

export function buildEip712SignatureAdviceEntry(
  pubkeyCommitment: Word,
  txSummaryCommitment: Word,
  signatureHex: string,
  publicKeyHex: string,
): { key: Word; values: Felt[] } {
  const expectedCommitment = tryComputeEcdsaCommitmentHex(publicKeyHex);
  if (expectedCommitment !== normalizeHexWord(pubkeyCommitment.toHex())) {
    throw new Error('EIP-712 public key commitment mismatch');
  }
  const publicKey = secp256k1.ProjectivePoint.fromHex(publicKeyHex.replace(/^0x/i, ''));
  const uncompressed = publicKey.toRawBytes(false);
  const normalizedSignature = EcdsaFormat.normalizeRecoveryByte(signatureHex);
  if (!/^0x[0-9a-fA-F]{130}$/.test(normalizedSignature)) {
    throw new Error('EIP-712 signature must be 65 hex-encoded bytes');
  }
  const signature = hexToBytes(normalizedSignature);
  if (signature.length !== 65 || (signature[64] !== 0 && signature[64] !== 1)) {
    throw new Error('EIP-712 signature must contain a valid recovery ID');
  }
  const typedData = midenTransactionTypedData(wordToBytes(txSummaryCommitment));
  if (!secp256k1.verify(signature.slice(0, 64), typedDataDigest(typedData), publicKey.toRawBytes(true))) {
    throw new Error('EIP-712 signature does not match the transaction summary');
  }

  const rawKey = Poseidon2.hashElements(new FeltArray([
    ...pubkeyCommitment.toFelts(),
    ...txSummaryCommitment.toFelts(),
  ]));
  const domain = new Word(new BigUint64Array([EIP712_SIGNATURE_KEY_DOMAIN, 0n, 0n, 0n]));
  const key = Poseidon2.hashElements(new FeltArray([...rawKey.toFelts(), ...domain.toFelts()]));
  return {
    key,
    values: [
      ...littleEndianU32Limbs(uncompressed.slice(1, 33)),
      ...littleEndianU32Limbs(uncompressed.slice(33, 65)),
      ...littleEndianU32Limbs(signature.slice(0, 32)),
      ...littleEndianU32Limbs(signature.slice(32, 64)),
    ],
  };
}

/** Rejects unrecoverable ECDSA signatures before entering WASM. */
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
