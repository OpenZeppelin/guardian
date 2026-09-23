import { describe, expect, it } from 'vitest';
import { secp256k1 } from '@noble/curves/secp256k1';
import { Word } from '@miden-sdk/miden-sdk';
import { bytesToHex } from './encoding.js';
import { midenTransactionTypedData, typedDataDigest } from './eip712.js';
import { buildEip712SignatureAdviceEntry, tryComputeEcdsaCommitmentHex } from './signature.js';
import { wordToBytes } from './word.js';

describe('EIP-712 transaction advice', () => {
  it('uses the enrolled key and the signed summary to build a 32-felt witness', () => {
    const privateKey = new Uint8Array(32).fill(7);
    const publicKey = bytesToHex(secp256k1.getPublicKey(privateKey, true));
    const commitmentHex = tryComputeEcdsaCommitmentHex(publicKey);
    expect(commitmentHex).not.toBeNull();
    const commitment = Word.fromHex(commitmentHex!);
    const summary = Word.fromHex('0x' + '01'.repeat(32));
    const typedData = midenTransactionTypedData(wordToBytes(summary));
    const signature = secp256k1.sign(typedDataDigest(typedData), privateKey);
    const signatureHex = bytesToHex(
      new Uint8Array([...signature.toCompactRawBytes(), signature.recovery + 27]),
    );

    const advice = buildEip712SignatureAdviceEntry(commitment, summary, signatureHex, publicKey);
    expect(advice.values).toHaveLength(32);
    expect(advice.key.toHex()).toMatch(/^0x[0-9a-f]{64}$/);
    const x = secp256k1.getPublicKey(privateKey, false).slice(1, 33);
    expect(advice.values[0].asInt()).toBe(BigInt(
      (x[28] << 24) | (x[29] << 16) | (x[30] << 8) | x[31],
    ) & 0xffff_ffffn);

    expect(() => buildEip712SignatureAdviceEntry(
      commitment,
      Word.fromHex('0x' + '02'.repeat(32)),
      signatureHex,
      publicKey,
    )).toThrow('does not match');
    expect(() => buildEip712SignatureAdviceEntry(
      commitment,
      summary,
      signatureHex,
      bytesToHex(secp256k1.getPublicKey(new Uint8Array(32).fill(8), true)),
    )).toThrow('commitment mismatch');
    expect(() => buildEip712SignatureAdviceEntry(
      commitment,
      summary,
      `${signatureHex.slice(0, -2)}zz`,
      publicKey,
    )).toThrow('hex-encoded bytes');
  });
});
