/**
 * Falcon Signer implementation for PSM client authentication.
 *
 * This implements the Signer interface using the Miden SDK's AuthSecretKey
 * for Falcon signatures.
 */

import { AuthSecretKey, Word, AccountId, Felt, FeltArray, Rpo256 } from '@miden-sdk/miden-sdk';
import type { Signer } from './types.js';
import { RequestAuthPayload } from '@openzeppelin/psm-client';
import { bytesToHex } from './utils/encoding.js';

class RequestAuthDigest {
  private constructor(private readonly payload: RequestAuthPayload) {}

  static fromPayload(payload: RequestAuthPayload): RequestAuthDigest {
    return new RequestAuthDigest(payload);
  }

  toWord(): Word {
    const bytes = this.payload.toBytes();
    if (bytes.length === 0) {
      return Word.fromHex(`0x${'0'.repeat(64)}`);
    }

    const elements: Felt[] = [];
    for (let offset = 0; offset < bytes.length; offset += 8) {
      let value = 0n;
      const chunk = bytes.subarray(offset, Math.min(offset + 8, bytes.length));
      for (let i = 0; i < chunk.length; i += 1) {
        value |= BigInt(chunk[i]) << BigInt(i * 8);
      }
      elements.push(new Felt(value));
    }

    return Rpo256.hashElements(new FeltArray(elements));
  }
}

/**
 * A Falcon signer that implements the Signer interface.
 *
 * @example
 * ```typescript
 * const secretKey = AuthSecretKey.rpoFalconWithRNG(seed);
 * const signer = new FalconSigner(secretKey);
 * console.log(signer.commitment);
 * ```
 */
export class FalconSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  private readonly secretKey: AuthSecretKey;

  constructor(secretKey: AuthSecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    this.commitment = pubKey.toCommitment().toHex();
    const serialized = pubKey.serialize();
    const falconPubKey = serialized.slice(1);
    this.publicKey = bytesToHex(falconPubKey);
  }

  signRequest(accountId: string, timestamp: number, requestPayload: RequestAuthPayload): string {
    const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
    const parsedAccountId = AccountId.fromHex(paddedHex);
    const prefix = parsedAccountId.prefix();
    const suffix = parsedAccountId.suffix();
    const payloadWord = RequestAuthDigest.fromPayload(requestPayload).toWord();
    const payloadFelts = payloadWord.toFelts();

    const feltArray = new FeltArray([
      prefix,
      suffix,
      new Felt(BigInt(timestamp)),
      payloadFelts[0],
      payloadFelts[1],
      payloadFelts[2],
      payloadFelts[3],
    ]);

    const digest = Rpo256.hashElements(feltArray);
    const signature = this.secretKey.sign(digest);
    const signatureBytes = signature.serialize();
    const falconSignature = signatureBytes.slice(1);
    return bytesToHex(falconSignature);
  }

  /**
   * Signs a commitment/word for proposal signing.
   */
  signCommitment(commitmentHex: string): string {
    const paddedHex = commitmentHex.startsWith('0x') ? commitmentHex : `0x${commitmentHex}`;
    const cleanHex = paddedHex.slice(2).padStart(64, '0');
    const word = Word.fromHex(`0x${cleanHex}`);
    const signature = this.secretKey.sign(word);
    const signatureBytes = signature.serialize();
    const falconSignature = signatureBytes.slice(1);
    return bytesToHex(falconSignature);
  }
}
