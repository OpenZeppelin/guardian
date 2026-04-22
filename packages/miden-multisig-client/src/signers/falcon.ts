import type { RequestAuthPayload } from '@openzeppelin/guardian-client';
import { AccountId, AuthSecretKey, type MidenClient, type Word } from '@miden-sdk/miden-sdk';
import type { Signer, SignatureScheme } from '../types.js';
import { bytesToHex } from '../utils/encoding.js';
import { AuthDigest } from '../utils/digest.js';

export class FalconSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme = 'falcon';
  private readonly secretKey: AuthSecretKey;
  private readonly publicKeyCommitment: Word;

  constructor(secretKey: AuthSecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    this.publicKeyCommitment = pubKey.toCommitment();
    this.commitment = this.publicKeyCommitment.toHex();
    const serialized = pubKey.serialize();
    const falconPubKey = serialized.slice(1);
    this.publicKey = bytesToHex(falconPubKey);
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

  async bindAccountKey(midenClient: MidenClient, accountId: string): Promise<void> {
    const targetAccountId = AccountId.fromHex(accountId);
    const existingAccountId = await midenClient.keystore.getAccountId(this.publicKeyCommitment);
    if (existingAccountId) {
      if (existingAccountId.toString().toLowerCase() === accountId.toLowerCase()) {
        return;
      }
      throw new Error(
        `Signer commitment ${this.commitment} is already bound to account ${existingAccountId.toString()}`,
      );
    }
    await midenClient.keystore.insert(targetAccountId, this.secretKey);
  }

  private signWord(word: Word): string {
    const signature = this.secretKey.sign(word);
    const signatureBytes = signature.serialize();
    const falconSignature = signatureBytes.slice(1);
    return bytesToHex(falconSignature);
  }
}
