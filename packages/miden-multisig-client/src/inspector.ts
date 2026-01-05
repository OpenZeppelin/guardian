/**
 * Account Inspector - Inspects account storage to detect multisig configuration.
 */

import { Account, Word } from '@demox-labs/miden-sdk';
import { base64ToUint8Array } from './utils/encoding.js';
import { wordElementToBigInt, wordToHex } from './utils/word.js';

export interface VaultBalance {
  faucetId: string;
  amount: bigint;
}

export interface DetectedMultisigConfig {
  threshold: number;
  numSigners: number;
  signerCommitments: string[];
  psmEnabled: boolean;
  psmCommitment: string | null;
  vaultBalances: VaultBalance[];
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

    const slot0 = storage.getItem(0) as Word;
    const threshold = Number(wordElementToBigInt(slot0, 0));
    const numSigners = Number(wordElementToBigInt(slot0, 1));

    const signerCommitments: string[] = [];
    for (let i = 0; i < numSigners; i++) {
      try {
        const key = new Word(new BigUint64Array([BigInt(i), 0n, 0n, 0n]));
        const commitment = storage.getMapItem(1, key) as Word;
        if (commitment) {
          signerCommitments.push(wordToHex(commitment));
        }
      } catch (error) {
        console.warn(error);
      }
    }

    let psmEnabled = false;
    let psmCommitment: string | null = null;

    try {
      const psmSlot0 = storage.getItem(4) as Word;
      const selector = Number(wordElementToBigInt(psmSlot0, 0));
      psmEnabled = selector === 1;

      if (psmEnabled) {
        const zeroKey = new Word(new BigUint64Array([0n, 0n, 0n, 0n]));
        const psmKey = storage.getMapItem(5, zeroKey) as Word;
        if (psmKey) {
          psmCommitment = wordToHex(psmKey);
        }
      }
    } catch (error) {
      console.warn(error);
    }

    const vaultBalances: VaultBalance[] = [];
    try {
      const vault = account.vault();
      const fungibleAssets = vault.fungibleAssets();
      for (const asset of fungibleAssets) {
        vaultBalances.push({
          faucetId: asset.faucetId().toString(),
          amount: BigInt(asset.amount()),
        });
      }
    } catch (error) {
      console.warn(error);
    }

    return {
      threshold,
      numSigners,
      signerCommitments,
      psmEnabled,
      psmCommitment,
      vaultBalances,
    };
  }
}
