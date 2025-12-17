/**
 * Account Inspector - Inspects account storage to detect multisig configuration.
 *
 * This utility parses serialized account data to extract the multisig
 * configuration (threshold, signers, PSM settings) from storage slots.
 */

import { Account, Word } from '@demox-labs/miden-sdk';

/**
 * Detected multisig configuration from account storage.
 */
export interface DetectedMultisigConfig {
  /** Number of signatures required to execute a transaction */
  threshold: number;
  /** Total number of signers in the multisig */
  numSigners: number;
  /** Public key commitments of all signers (hex strings) */
  signerCommitments: string[];
  /** Whether PSM verification is enabled */
  psmEnabled: boolean;
  /** PSM server public key commitment (hex string, if enabled) */
  psmCommitment: string | null;
}

/**
 * Convert base64 string to Uint8Array.
 */
function base64ToUint8Array(base64: string): Uint8Array {
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

/**
 * Convert a Word to hex string.
 */
function wordToHex(word: Word): string {
  return word.toHex();
}

/**
 * Extract a BigInt from a Word at a specific element index.
 * Word stores 4 x u64 values, accessed via toU64s().
 */
function wordElementToBigInt(word: Word, index: number): bigint {
  const u64s = word.toU64s();
  if (index >= 0 && index < u64s.length) {
    return u64s[index];
  }
  return 0n;
}

/**
 * Inspects an account to detect its multisig configuration.
 *
 * @example
 * ```typescript
 * // From base64-encoded state
 * const config = AccountInspector.fromBase64(stateDataBase64);
 * console.log(`${config.threshold}-of-${config.numSigners} multisig`);
 *
 * // From Miden SDK Account
 * const config = AccountInspector.fromAccount(account);
 * ```
 */
export class AccountInspector {
  private constructor() {}

  /**
   * Inspect a base64-encoded serialized account.
   *
   * @param base64Data - Base64-encoded Account bytes
   * @returns Detected multisig configuration
   */
  static fromBase64(base64Data: string): DetectedMultisigConfig {
    const bytes = base64ToUint8Array(base64Data);
    const account = Account.deserialize(bytes);
    return AccountInspector.fromAccount(account);
  }

  /**
   * Inspect a Miden SDK Account object.
   *
   * @param account - The Account object from Miden SDK
   * @returns Detected multisig configuration
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  static fromAccount(account: Account): DetectedMultisigConfig {
    const storage = account.storage();

    // Get storage slots
    // Multisig component: slots 0-3
    // PSM component: slots 4-5 (if present)

    // Slot 0: Threshold config [threshold, num_signers, 0, 0]
    const slot0 = storage.getItem(0) as Word;
    const threshold = Number(wordElementToBigInt(slot0, 0));
    const numSigners = Number(wordElementToBigInt(slot0, 1));

    // Slot 1: Signer commitments map
    // Read signers by iterating through indices 0..numSigners
    const signerCommitments: string[] = [];
    for (let i = 0; i < numSigners; i++) {
      try {
        // Create key [index, 0, 0, 0]
        const key = new Word(new BigUint64Array([BigInt(i), 0n, 0n, 0n]));
        const commitment = storage.getMapItem(1, key) as Word;
        if (commitment) {
          signerCommitments.push(wordToHex(commitment));
        }
      } catch {
        // Map entry might not exist
      }
    }

    // PSM slots (offset by 4 from multisig slots)
    // Slot 4: PSM selector [1, 0, 0, 0] for ON, [0, 0, 0, 0] for OFF
    let psmEnabled = false;
    let psmCommitment: string | null = null;

    try {
      const psmSlot0 = storage.getItem(4) as Word;
      const selector = Number(wordElementToBigInt(psmSlot0, 0));
      psmEnabled = selector === 1;

      if (psmEnabled) {
        // Slot 5: PSM public key map
        const zeroKey = new Word(new BigUint64Array([0n, 0n, 0n, 0n]));
        const psmKey = storage.getMapItem(5, zeroKey) as Word;
        if (psmKey) {
          psmCommitment = wordToHex(psmKey);
        }
      }
    } catch {
      // PSM component might not be present
    }

    return {
      threshold,
      numSigners,
      signerCommitments,
      psmEnabled,
      psmCommitment,
    };
  }
}
