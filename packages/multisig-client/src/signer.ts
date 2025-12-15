/**
 * Falcon Signer implementation for PSM client authentication.
 *
 * This implements the Signer interface using the Miden SDK's SecretKey
 * for Falcon signatures. Mirrors the KeyManager trait from Rust.
 */

import { SecretKey, Word } from '@demox-labs/miden-sdk';
import type { Signer } from './types.js';
import type { KeyEntry } from './keystore.js';
import { loadSecretKey } from './keystore.js';

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
// Signer Creation
// =============================================================================

/**
 * Creates a Signer from a KeyEntry using the Miden SDK.
 *
 * @param keyEntry - The key entry from the keystore
 * @returns A Signer implementation
 */
export function createSigner(keyEntry: KeyEntry): Signer {
  // Load the secret key from the entry
  const secretKey = loadSecretKey(keyEntry);
  const publicKey = secretKey.publicKey();

  // Get the commitment (used for authorization checks)
  const commitment = publicKey.toCommitment().toHex();

  // Get the full public key bytes for the x-pubkey header
  const publicKeyBytes = publicKey.serialize();
  const publicKeyHex = bytesToHex(publicKeyBytes);

  return {
    commitment,
    publicKey: publicKeyHex,

    /**
     * Signs an account ID for request authentication.
     * The server verifies this signature to authorize the request.
     *
     * Account ID is converted to a Word (padded/hashed as needed) and signed.
     */
    signAccountId(accountId: string): string {
      // Convert account ID hex string to a Word for signing
      // Account IDs are 15 bytes (30 hex chars), we need to create a proper Word
      // A Word is 4 field elements (32 bytes). We'll use Word.fromHex with padding.
      const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
      // Pad to 64 chars (32 bytes = 4 field elements)
      const cleanHex = paddedHex.slice(2).padStart(64, '0');
      const word = Word.fromHex(cleanHex);

      // Sign the word
      const signature = secretKey.sign(word);

      // Serialize the signature to bytes and convert to hex
      const signatureBytes = signature.serialize();
      return bytesToHex(signatureBytes);
    },

    /**
     * Signs a commitment/word for proposal signing.
     * Used when signing delta proposals.
     */
    signCommitment(commitmentHex: string): string {
      // Convert commitment hex string to a Word for signing
      // Commitments are typically 32 bytes (64 hex chars)
      const paddedHex = commitmentHex.startsWith('0x') ? commitmentHex : `0x${commitmentHex}`;
      const cleanHex = paddedHex.slice(2).padStart(64, '0');
      const word = Word.fromHex(cleanHex);

      // Sign the word
      const signature = secretKey.sign(word);

      // Serialize the signature to bytes and convert to hex
      const signatureBytes = signature.serialize();
      return bytesToHex(signatureBytes);
    },
  };
}

/**
 * Creates a Signer directly from a secret key instance.
 *
 * @param secretKey - The secret key instance
 * @returns A Signer implementation
 */
export function createSignerFromSecretKey(secretKey: SecretKey): Signer {
  const publicKey = secretKey.publicKey();

  // Get the commitment (used for authorization checks)
  const commitment = publicKey.toCommitment().toHex();

  // Get the full public key bytes for the x-pubkey header
  const publicKeyBytes = publicKey.serialize();
  const publicKeyHex = bytesToHex(publicKeyBytes);

  return {
    commitment,
    publicKey: publicKeyHex,

    signAccountId(accountId: string): string {
      const paddedHex = accountId.startsWith('0x') ? accountId : `0x${accountId}`;
      const cleanHex = paddedHex.slice(2).padStart(64, '0');
      const word = Word.fromHex(cleanHex);
      const signature = secretKey.sign(word);
      const signatureBytes = signature.serialize();
      return bytesToHex(signatureBytes);
    },

    signCommitment(commitmentHex: string): string {
      const paddedHex = commitmentHex.startsWith('0x') ? commitmentHex : `0x${commitmentHex}`;
      const cleanHex = paddedHex.slice(2).padStart(64, '0');
      const word = Word.fromHex(cleanHex);
      const signature = secretKey.sign(word);
      const signatureBytes = signature.serialize();
      return bytesToHex(signatureBytes);
    },
  };
}
