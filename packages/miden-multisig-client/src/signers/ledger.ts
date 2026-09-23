import { secp256k1 } from '@noble/curves/secp256k1';
import { keccak_256 } from '@noble/hashes/sha3.js';
import type { RequestAuthPayload, Signer } from '@openzeppelin/guardian-client';
import { AuthDigest } from '../utils/digest.js';
import { EcdsaFormat } from '../utils/ecdsa.js';
import { bytesToHex, hexToBytes } from '../utils/encoding.js';
import {
  guardianKeyDiscoveryTypedData,
  guardianRequestTypedData,
  midenTransactionTypedData,
  typedDataDigest,
} from '../utils/eip712.js';
import { tryComputeEcdsaCommitmentHex } from '../utils/signature.js';
import { wordToBytes } from '../utils/word.js';

export interface Eip1193SignerProvider {
  request(args: { method: string; params: unknown[] }): Promise<unknown>;
}

export class LedgerSigner implements Signer {
  readonly scheme = 'ecdsa';
  readonly requestAuthFormat = 'eip712';
  readonly proposalMessageFormat = 'eip712';
  readonly publicKey: string;
  readonly commitment: string;
  readonly address: string;

  static async connect(provider: Eip1193SignerProvider): Promise<LedgerSigner> {
    const accounts = await provider.request({ method: 'eth_requestAccounts', params: [] });
    if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') {
      throw new Error('Ledger did not provide an Ethereum address');
    }
    const address = accounts[0];
    const challenge = crypto.getRandomValues(new Uint8Array(32));
    const data = guardianKeyDiscoveryTypedData(challenge);
    const result = await provider.request({
      method: 'eth_signTypedData_v4',
      params: [address, JSON.stringify(data)],
    });
    if (typeof result !== 'string' || !/^0x[0-9a-fA-F]{130}$/.test(result)) {
      throw new Error('Ledger returned an invalid key-discovery signature');
    }
    const signature = hexToBytes(EcdsaFormat.normalizeRecoveryByte(result));
    if (signature[64] !== 0 && signature[64] !== 1) {
      throw new Error('Ledger returned an invalid recovery ID');
    }
    const publicKey = secp256k1.Signature.fromCompact(signature.slice(0, 64))
      .addRecoveryBit(signature[64])
      .recoverPublicKey(typedDataDigest(data))
      .toRawBytes(true);
    return new LedgerSigner(provider, bytesToHex(publicKey), address);
  }

  constructor(
    private readonly provider: Eip1193SignerProvider,
    publicKey: string,
    address: string,
  ) {
    if (!EcdsaFormat.isValidPublicKeyPoint(publicKey)) {
      throw new Error('Invalid Ledger public key');
    }
    this.publicKey = EcdsaFormat.compressPublicKey(publicKey);
    const commitment = tryComputeEcdsaCommitmentHex(this.publicKey);
    if (!commitment) {
      throw new Error('Cannot derive the Miden commitment for the Ledger public key');
    }
    this.commitment = commitment;

    const uncompressed = secp256k1.ProjectivePoint.fromHex(this.publicKey.slice(2)).toRawBytes(false);
    const derivedAddress = bytesToHex(keccak_256(uncompressed.slice(1)).slice(-20));
    if (derivedAddress.toLowerCase() !== address.toLowerCase()) {
      throw new Error('Ledger address does not match its public key');
    }
    this.address = address;
  }

  signAccountIdWithTimestamp(): Promise<string> {
    throw new Error('LedgerSigner requires request-bound Guardian authentication');
  }

  async signRequest(
    accountId: string,
    timestamp: number,
    requestPayload: RequestAuthPayload,
  ): Promise<string> {
    const requestHash = AuthDigest.fromRequest(accountId, timestamp, requestPayload);
    return this.signTypedData(guardianRequestTypedData(wordToBytes(requestHash)));
  }

  async signCommitment(commitmentHex: string): Promise<string> {
    const commitment = AuthDigest.fromCommitmentHex(commitmentHex);
    return this.signTypedData(midenTransactionTypedData(wordToBytes(commitment)));
  }

  private async signTypedData(data: ReturnType<typeof guardianRequestTypedData>): Promise<string> {
    const result = await this.provider.request({
      method: 'eth_signTypedData_v4',
      params: [this.address, JSON.stringify(data)],
    });
    if (typeof result !== 'string' || !/^0x[0-9a-fA-F]{130}$/.test(result)) {
      throw new Error('Ledger returned an invalid EIP-712 signature');
    }
    const signatureHex = EcdsaFormat.normalizeRecoveryByte(result);
    const signature = hexToBytes(signatureHex);
    if (signature[64] !== 0 && signature[64] !== 1) {
      throw new Error('Ledger returned an invalid ECDSA recovery ID');
    }
    const recovered = secp256k1.Signature.fromCompact(signature.slice(0, 64))
      .addRecoveryBit(signature[64])
      .recoverPublicKey(typedDataDigest(data))
      .toRawBytes(true);
    if (bytesToHex(recovered).toLowerCase() !== this.publicKey.toLowerCase()) {
      throw new Error('Ledger signature was produced by a different key');
    }
    return signatureHex;
  }
}
