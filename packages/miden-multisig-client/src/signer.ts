/**
 * Falcon Signer implementation for PSM client authentication.
 *
 * This implements the Signer interface using the Miden SDK's SecretKey
 * for Falcon signatures.
 */

import { SecretKey, Word, AccountId, Felt, FeltArray, Rpo256 } from '@demox-labs/miden-sdk';
import type { Signer } from './types.js';

// =============================================================================
// Utilities
// =============================================================================

/**
 * Converts a Uint8Array to a hex string with 0x prefix.
 */
function bytesToHex(bytes: Uint8Array): string {
  let hex = '0x';
  for (let i = 0; i < bytes.length; i++) {
    hex += bytes[i].toString(16).padStart(2, '0');
  }
  return hex;
}

// =============================================================================
// FalconSigner Class
// =============================================================================

/**
 * A Falcon signer that implements the Signer interface.
 *
 * @example
 * ```typescript
 * const secretKey = SecretKey.rpoFalconWithRNG(seed);
 * const signer = new FalconSigner(secretKey);
 * console.log(signer.commitment);
 * ```
 */
export class FalconSigner implements Signer {
  readonly commitment: string;
  readonly publicKey: string;
  private readonly secretKey: SecretKey;

  constructor(secretKey: SecretKey) {
    this.secretKey = secretKey;
    const pubKey = secretKey.publicKey();
    this.commitment = pubKey.toCommitment().toHex();
    this.publicKey = bytesToHex(pubKey.serialize());
  }

  /**
   * Signs an account ID for request authentication.
   * The server verifies this signature to authorize the request.
   *
   * This mirrors the Rust server's account_id_to_digest function:
   * 1. Parse the account ID from hex
   * 2. Convert to field elements [prefix, suffix]
   * 3. Pad with zeros to 4 elements
   * 4. Hash using RPO256 to produce the message digest
   */
  signAccountId(accountId: string): string {
    // Parse the account ID from hex
    const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
    const parsedAccountId = AccountId.fromHex(paddedHex);

    // Convert AccountId to its field element representation [prefix, suffix]
    const prefix = parsedAccountId.prefix();
    const suffix = parsedAccountId.suffix();

    // We use 4 elements to fill a full rate (pad with zeros)
    const feltArray = new FeltArray([
      prefix,
      suffix,
      new Felt(BigInt(0)),
      new Felt(BigInt(0)),
    ]);

    // Hash using RPO256 and sign the digest
    const digest = Rpo256.hashElements(feltArray);
    const signature = this.secretKey.sign(digest);
    const signatureBytes = signature.serialize();
    return bytesToHex(signatureBytes);
  }

  /**
   * Signs a commitment/word for proposal signing.
   * Used when signing delta proposals.
   */
  signCommitment(commitmentHex: string): string {
    const paddedHex = commitmentHex.startsWith('0x') ? commitmentHex : `0x${commitmentHex}`;
    const cleanHex = paddedHex.slice(2).padStart(64, '0');
    const word = Word.fromHex(cleanHex);
    const signature = this.secretKey.sign(word);
    const signatureBytes = signature.serialize();
    return bytesToHex(signatureBytes);
  }
}
