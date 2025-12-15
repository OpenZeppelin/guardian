/**
 * Storage slot builders for multisig accounts.
 *
 * Mirrors the storage layout from the Rust MultisigPsmBuilder.
 */

import type { MultisigConfig } from '../types.js';
import {
  StorageSlot,
  StorageMap,
  Word,
} from '@demox-labs/miden-sdk';

// =============================================================================
// Storage Slot Builders
// =============================================================================

/**
 * Builds the multisig component storage slots (4 slots).
 *
 * Storage Layout:
 * - Slot 0: Threshold config [threshold, num_signers, 0, 0]
 * - Slot 1: Signer public keys map [index, 0, 0, 0] => COMMITMENT
 * - Slot 2: Executed transactions map (empty, for replay protection)
 * - Slot 3: Procedure threshold overrides map (empty)
 *
 * @param config - Multisig configuration
 * @returns Array of 4 storage slots
 */
export function buildMultisigStorageSlots(config: MultisigConfig): StorageSlot[] {
  const numSigners = config.signerCommitments.length;

  // Slot 0: Threshold config [threshold, num_signers, 0, 0]
  const slot0Word = new Word(
    new BigUint64Array([
      BigInt(config.threshold),
      BigInt(numSigners),
      0n,
      0n,
    ])
  );
  const slot0 = StorageSlot.fromValue(slot0Word);

  // Slot 1: Signer public keys map
  const signersMap = new StorageMap();
  config.signerCommitments.forEach((commitment, index) => {
    const key = new Word(new BigUint64Array([BigInt(index), 0n, 0n, 0n]));
    const value = Word.fromHex(commitment);
    signersMap.insert(key, value);
  });
  const slot1 = StorageSlot.map(signersMap);

  // Slot 2: Executed transactions map (empty)
  const slot2 = StorageSlot.map(new StorageMap());

  // Slot 3: Procedure threshold overrides (empty)
  const slot3 = StorageSlot.map(new StorageMap());

  return [slot0, slot1, slot2, slot3];
}

/**
 * Builds the PSM component storage slots (2 slots).
 *
 * Storage Layout:
 * - Slot 0: PSM selector [1, 0, 0, 0] for ON, [0, 0, 0, 0] for OFF
 * - Slot 1: PSM public key map [0, 0, 0, 0] => PSM_COMMITMENT
 *
 * @param config - Multisig configuration
 * @returns Array of 2 storage slots
 */
export function buildPsmStorageSlots(config: MultisigConfig): StorageSlot[] {
  // Slot 0: PSM selector
  const selector = config.psmEnabled !== false ? 1n : 0n;
  const selectorWord = new Word(new BigUint64Array([selector, 0n, 0n, 0n]));
  const slot0 = StorageSlot.fromValue(selectorWord);

  // Slot 1: PSM public key map
  const psmKeyMap = new StorageMap();
  const zeroKey = new Word(new BigUint64Array([0n, 0n, 0n, 0n]));
  const psmKey = Word.fromHex(config.psmCommitment);
  psmKeyMap.insert(zeroKey, psmKey);
  const slot1 = StorageSlot.map(psmKeyMap);

  return [slot0, slot1];
}
