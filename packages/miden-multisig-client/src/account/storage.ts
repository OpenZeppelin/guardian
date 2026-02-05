import type { MultisigConfig } from '../types.js';
import { StorageSlot, StorageMap, Word } from '@demox-labs/miden-sdk';
import { normalizeHexWord } from '../utils/encoding.js';
import { getProcedureRoot } from '../procedures.js';

export class StorageLayoutBuilder {
  buildMultisigSlots(config: MultisigConfig): StorageSlot[] {
    const numSigners = config.signerCommitments.length;
    const slot0Word = new Word(
      new BigUint64Array([
        BigInt(config.threshold),
        BigInt(numSigners),
        0n,
        0n,
      ])
    );
    const slot0 = StorageSlot.fromValue(slot0Word);

    const signersMap = new StorageMap();
    config.signerCommitments.forEach((commitment, index) => {
      const key = new Word(new BigUint64Array([BigInt(index), 0n, 0n, 0n]));
      const value = Word.fromHex(normalizeHexWord(commitment));
      signersMap.insert(key, value);
    });
    const slot1 = StorageSlot.map(signersMap);

    const slot2 = StorageSlot.map(new StorageMap());

    const procThresholdMap = new StorageMap();
    if (config.procedureThresholds) {
      const signatureScheme = config.signatureScheme ?? 'falcon';
      for (const pt of config.procedureThresholds) {
        const rootHex = getProcedureRoot(pt.procedure, signatureScheme);
        const key = Word.fromHex(rootHex);
        const value = new Word(new BigUint64Array([BigInt(pt.threshold), 0n, 0n, 0n]));
        procThresholdMap.insert(key, value);
      }
    }
    const slot3 = StorageSlot.map(procThresholdMap);

    return [slot0, slot1, slot2, slot3];
  }

  buildPsmSlots(config: MultisigConfig): StorageSlot[] {
    const selector = config.psmEnabled !== false ? 1n : 0n;
    const selectorWord = new Word(new BigUint64Array([selector, 0n, 0n, 0n]));
    const slot0 = StorageSlot.fromValue(selectorWord);

    const psmKeyMap = new StorageMap();
    const zeroKey = new Word(new BigUint64Array([0n, 0n, 0n, 0n]));
    const psmKey = Word.fromHex(normalizeHexWord(config.psmCommitment));
    psmKeyMap.insert(zeroKey, psmKey);
    const slot1 = StorageSlot.map(psmKeyMap);

    return [slot0, slot1];
  }
}

const defaultStorageBuilder = new StorageLayoutBuilder();

export function buildMultisigStorageSlots(config: MultisigConfig): StorageSlot[] {
  return defaultStorageBuilder.buildMultisigSlots(config);
}

export function buildPsmStorageSlots(config: MultisigConfig): StorageSlot[] {
  return defaultStorageBuilder.buildPsmSlots(config);
}

export const storageLayoutBuilder = defaultStorageBuilder;
