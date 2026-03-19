import type { RequestAuthPayload } from '@openzeppelin/guardian-client';
import { AuthSecretKey } from '@miden-sdk/miden-sdk';
import type { Signer, SignatureScheme } from '../types.js';
import { bytesToHex, normalizeHexWord } from '../utils/encoding.js';
import { AuthDigest } from '../utils/digest.js';

export class EcdsaSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme = 'ecdsa';
  private readonly secretKey: AuthSecretKey;

  constructor(secretKey: AuthSecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    const serialized = pubKey.serialize();
    this.publicKey = bytesToHex(serialized.slice(1));
    this.commitment = normalizeHexWord(pubKey.toCommitment().toHex());
  }

  async signAccountIdWithTimestamp(accountId: string, timestamp: number): Promise<string> {
    const digest = AuthDigest.fromAccountIdWithTimestamp(accountId, timestamp);
    return this.signWord(digest);
  }

  async signRequest(
    accountId: string,
    timestamp: number,
    requestPayload: RequestAuthPayload,
  ): Promise<string> {
    const digest = AuthDigest.fromRequest(accountId, timestamp, requestPayload);
    return this.signWord(digest);
  }

  async signCommitment(commitmentHex: string): Promise<string> {
    const word = AuthDigest.fromCommitmentHex(commitmentHex);
    return this.signWord(word);
  }

  private signWord(word: ReturnType<typeof AuthDigest.fromCommitmentHex>): string {
    const signature = this.secretKey.sign(word);
    const signatureBytes = signature.serialize();
    const ecdsaSignature = signatureBytes.slice(1);
    return bytesToHex(ecdsaSignature);
  }
}
