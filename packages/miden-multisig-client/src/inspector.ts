import { Account, Word } from '@miden-sdk/miden-sdk';
import { base64ToUint8Array } from './utils/encoding.js';
import { wordElementToBigInt, wordToHex } from './utils/word.js';
import { getProcedureRoot, getProcedureNames, type ProcedureName } from './procedures.js';
import type { SignatureScheme } from './types.js';

const MULTISIG_SLOT_NAMES = {
  THRESHOLD_CONFIG: 'openzeppelin::multisig::threshold_config',
  SIGNER_PUBLIC_KEYS: 'openzeppelin::multisig::signer_public_keys',
  EXECUTED_TRANSACTIONS: 'openzeppelin::multisig::executed_transactions',
  PROCEDURE_THRESHOLDS: 'openzeppelin::multisig::procedure_thresholds',
} as const;

const PSM_SLOT_NAMES = {
  SELECTOR: 'openzeppelin::psm::selector',
  PUBLIC_KEY: 'openzeppelin::psm::public_key',
} as const;

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

    const slot0 = storage.getItem(MULTISIG_SLOT_NAMES.THRESHOLD_CONFIG) as Word;
    const threshold = Number(wordElementToBigInt(slot0, 0));
    const numSigners = Number(wordElementToBigInt(slot0, 1));

    const signerCommitments: string[] = [];
    for (let i = 0; i < numSigners; i++) {
      try {
        const key = new Word(new BigUint64Array([BigInt(i), 0n, 0n, 0n]));
        const commitment = storage.getMapItem(MULTISIG_SLOT_NAMES.SIGNER_PUBLIC_KEYS, key) as Word;
        if (commitment) {
          signerCommitments.push(wordToHex(commitment));
        }
      } catch {
      }
    }

    let psmEnabled = false;
    let psmCommitment: string | null = null;

    try {
      const psmSlot0 = storage.getItem(PSM_SLOT_NAMES.SELECTOR) as Word;
      const selector = Number(wordElementToBigInt(psmSlot0, 0));
      psmEnabled = selector === 1;

      if (psmEnabled) {
        const zeroKey = new Word(new BigUint64Array([0n, 0n, 0n, 0n]));
        const psmKey = storage.getMapItem(PSM_SLOT_NAMES.PUBLIC_KEY, zeroKey) as Word;
        if (psmKey) {
          psmCommitment = wordToHex(psmKey);
        }
      }
    } catch {
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
    }

    const procedureThresholds = new Map<ProcedureName, number>();
    for (const procName of getProcedureNames(signatureScheme)) {
      try {
        const rootHex = getProcedureRoot(procName, signatureScheme);
        const rootWord = Word.fromHex(rootHex);
        const value = storage.getMapItem(MULTISIG_SLOT_NAMES.PROCEDURE_THRESHOLDS, rootWord) as Word;
        if (value) {
          const procThreshold = Number(wordElementToBigInt(value, 0));
          if (procThreshold > 0) {
            procedureThresholds.set(procName, procThreshold);
          }
        }
      } catch {
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
