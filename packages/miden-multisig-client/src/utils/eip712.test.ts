import { describe, expect, it } from 'vitest';
import { secp256k1 } from '@noble/curves/secp256k1';
import { Word } from '@miden-sdk/miden-sdk';
import { hashTypedData } from 'viem';
import { bytesToHex, hexToBytes } from './encoding.js';
import { guardianLookupTypedData, guardianRequestTypedData, midenTransactionTypedData, typedDataDigest } from './eip712.js';
import { buildEip712SignatureAdviceEntry, tryComputeEcdsaCommitmentHex } from './signature.js';
import { wordToBytes } from './word.js';

describe('EIP-712 typed data and transaction advice', () => {
  it('accepts the protocol MetaMask signTypedData v4 vector', () => {
    const publicKey = '0x034f355bdcb7cc0af728ef3cceb9615d90684bb5b2ca5f859ab0f0b704075871aa';
    const signature = '0x13deb6c6f8903c58117a86d11ce7140af8a1d90dc9a25cb03d1c5972deae7ae8'
      + '06dda37b581ebcb290000fc2f460c964c5fbca5e6ad4b77109609db80743ca8d1b';
    const summary = Word.fromHex('0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef');
    const commitmentHex = tryComputeEcdsaCommitmentHex(publicKey);
    if (!commitmentHex) throw new Error('Could not derive the fixture public-key commitment');

    const advice = buildEip712SignatureAdviceEntry(
      Word.fromHex(commitmentHex), summary, signature, publicKey,
    );
    expect(advice.values).toHaveLength(32);
    expect(advice.key.toHex()).toMatch(/^0x[0-9a-f]{64}$/);
  });

  it('matches viem typed-data hashes and the Miden transaction vector', () => {
    const summaryHash = '0x0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';
    const hashBytes = hexToBytes(summaryHash);
    const transaction = midenTransactionTypedData(hashBytes);
    const request = guardianRequestTypedData(hashBytes);
    const lookup = guardianLookupTypedData(hashBytes);
    expect(bytesToHex(typedDataDigest(transaction))).toBe(hashTypedData(transaction));
    expect(bytesToHex(typedDataDigest(request))).toBe(hashTypedData(request));
    expect(bytesToHex(typedDataDigest(lookup))).toBe(hashTypedData(lookup));
    expect(hashTypedData(transaction))
      .toBe('0xf9a8d508052b86648521d4e701982acc3c038334ba5d55ca7f83cce48026955a');
  });

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
    const otherRecoveryBit = bytesToHex(new Uint8Array([
      ...signature.toCompactRawBytes(),
      (signature.recovery ^ 1) + 27,
    ]));
    const otherAdvice = buildEip712SignatureAdviceEntry(
      commitment, summary, otherRecoveryBit, publicKey,
    );
    expect(otherAdvice.key.toHex()).toBe(advice.key.toHex());
    expect(otherAdvice.values.map(value => value.asInt()))
      .toEqual(advice.values.map(value => value.asInt()));
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
