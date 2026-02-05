import { SecretKey, Word, AccountId, Felt, FeltArray, Rpo256 } from '@demox-labs/miden-sdk';
import type { Signer, SignatureScheme } from './types.js';
import { bytesToHex } from './utils/encoding.js';

export class FalconSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme = 'falcon';
  private readonly secretKey: SecretKey;

  constructor(secretKey: SecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    this.commitment = pubKey.toCommitment().toHex();
    const serialized = pubKey.serialize();
    const falconPubKey = serialized.slice(1);
    this.publicKey = bytesToHex(falconPubKey);
  }

  signAccountIdWithTimestamp(accountId: string, timestamp: number): string {
    const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
    const parsedAccountId = AccountId.fromHex(paddedHex);
    const prefix = parsedAccountId.prefix();
    const suffix = parsedAccountId.suffix();

    const feltArray = new FeltArray([
      prefix,
      suffix,
      new Felt(BigInt(timestamp)),
      new Felt(BigInt(0)),
    ]);

    const digest = Rpo256.hashElements(feltArray);
    const signature = this.secretKey.sign(digest);
    const signatureBytes = signature.serialize();
    const falconSignature = signatureBytes.slice(1);
    return bytesToHex(falconSignature);
  }

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

export class EcdsaSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  readonly scheme: SignatureScheme = 'ecdsa';
  private readonly secretKey: SecretKey;

  constructor(secretKey: SecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    this.commitment = pubKey.toCommitment().toHex();
    const serialized = pubKey.serialize();
    const ecdsaPubKey = serialized.slice(1);
    this.publicKey = bytesToHex(ecdsaPubKey);
  }

  signAccountIdWithTimestamp(accountId: string, timestamp: number): string {
    const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
    const parsedAccountId = AccountId.fromHex(paddedHex);
    const prefix = parsedAccountId.prefix();
    const suffix = parsedAccountId.suffix();

    const feltArray = new FeltArray([
      prefix,
      suffix,
      new Felt(BigInt(timestamp)),
      new Felt(BigInt(0)),
    ]);

    const digest = Rpo256.hashElements(feltArray);
    const signature = this.secretKey.sign(digest);
    const signatureBytes = signature.serialize();
    const ecdsaSignature = signatureBytes.slice(1);
    return bytesToHex(ecdsaSignature);
  }

  signCommitment(commitmentHex: string): string {
    const paddedHex = commitmentHex.startsWith('0x') ? commitmentHex : `0x${commitmentHex}`;
    const cleanHex = paddedHex.slice(2).padStart(64, '0');
    const word = Word.fromHex(`0x${cleanHex}`);
    const signature = this.secretKey.sign(word);
    const signatureBytes = signature.serialize();
    const ecdsaSignature = signatureBytes.slice(1);
    return bytesToHex(ecdsaSignature);
  }
}
