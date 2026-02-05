import { Account, Word } from '@demox-labs/miden-sdk';
import { base64ToUint8Array } from './utils/encoding.js';
import { wordElementToBigInt, wordToHex } from './utils/word.js';
import { getProcedureRoot, getProcedureNames, type ProcedureName } from './procedures.js';
import type { SignatureScheme } from './types.js';

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
  procedureThresholds: Map<ProcedureName, number>;
  signatureScheme: SignatureScheme;
}

export class AccountInspector {
  private constructor() {}

  static fromBase64(base64Data: string, signatureScheme: SignatureScheme = 'falcon'): DetectedMultisigConfig {
    const bytes = base64ToUint8Array(base64Data);
    const account = Account.deserialize(bytes);
    return AccountInspector.fromAccount(account, signatureScheme);
  }

  static fromAccount(account: Account, signatureScheme: SignatureScheme = 'falcon'): DetectedMultisigConfig {
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
      } catch {
        // Signer not found in storage map
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
    } catch {
      // PSM not configured
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
    } catch {
      // Vault access failed
    }

    const procedureThresholds = new Map<ProcedureName, number>();
    for (const procName of getProcedureNames(signatureScheme)) {
      try {
        const rootHex = getProcedureRoot(procName, signatureScheme);
        const rootWord = Word.fromHex(rootHex);
        const value = storage.getMapItem(3, rootWord) as Word;
        if (value) {
          const procThreshold = Number(wordElementToBigInt(value, 0));
          if (procThreshold > 0) {
            procedureThresholds.set(procName, procThreshold);
          }
        }
      } catch {
        // Not set
      }
    }

    return {
      threshold,
      numSigners,
      signerCommitments,
      psmEnabled,
      psmCommitment,
      vaultBalances,
      procedureThresholds,
      signatureScheme,
    };
  }
}
