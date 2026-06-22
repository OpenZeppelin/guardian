import type { MultisigConfig } from '../types.js';
import { StorageSlot, StorageMap, Word } from '@miden-sdk/miden-sdk';
import { ensureHexPrefix } from '../utils/encoding.js';
import { getProcedureRoot } from '../procedures.js';

// `AuthGuardedMultisig` storage slot names (miden::standards::auth::*). These must match the
// Rust `miden-standards` component exactly: account ID and commitment derive from the storage
// layout, so any divergence breaks cross-SDK determinism (guarded by the parity test).
const MULTISIG_SLOT_NAMES = {
  THRESHOLD_CONFIG: 'miden::standards::auth::multisig::threshold_config',
  SIGNER_PUBLIC_KEYS: 'miden::standards::auth::multisig::approver_public_keys',
  SIGNER_SCHEME_IDS: 'miden::standards::auth::multisig::approver_schemes',
  EXECUTED_TRANSACTIONS: 'miden::standards::auth::multisig::executed_transactions',
  PROCEDURE_THRESHOLDS: 'miden::standards::auth::multisig::procedure_thresholds',
} as const;

const GUARDIAN_SLOT_NAMES = {
  PUBLIC_KEY: 'miden::standards::auth::guardian::pub_key',
  SCHEME_ID: 'miden::standards::auth::guardian::scheme',
} as const;

function signerMapKey(index: bigint): Word {
  return new Word(new BigUint64Array([index, 0n, 0n, 0n]));
}

export class StorageLayoutBuilder {
  buildMultisigSlots(config: MultisigConfig): StorageSlot[] {
    const numSigners = config.signerCommitments.length;
    const schemeId = config.signatureScheme === 'ecdsa' ? 1n : 2n;
    const slot0Word = new Word(
      new BigUint64Array([
        BigInt(config.threshold),
        BigInt(numSigners),
        0n,
        0n,
      ])
    );
    const slot0 = StorageSlot.fromValue(MULTISIG_SLOT_NAMES.THRESHOLD_CONFIG, slot0Word);

    const signersMap = new StorageMap();
    config.signerCommitments.forEach((commitment, index) => {
      const key = signerMapKey(BigInt(index));
      const value = Word.fromHex(ensureHexPrefix(commitment));
      signersMap.insert(key, value);
    });
    const slot1 = StorageSlot.map(MULTISIG_SLOT_NAMES.SIGNER_PUBLIC_KEYS, signersMap);

    const signerSchemesMap = new StorageMap();
    config.signerCommitments.forEach((_, index) => {
      const key = signerMapKey(BigInt(index));
      const value = new Word(new BigUint64Array([schemeId, 0n, 0n, 0n]));
      signerSchemesMap.insert(key, value);
    });
    const slot2 = StorageSlot.map(MULTISIG_SLOT_NAMES.SIGNER_SCHEME_IDS, signerSchemesMap);

    const slot3 = StorageSlot.map(MULTISIG_SLOT_NAMES.EXECUTED_TRANSACTIONS, new StorageMap());

    // Map entries: PROC_ROOT => [proc_threshold, 0, 0, 0]
    // Use SDK's Word.fromHex to match how account code procedure roots are represented
    const procThresholdMap = new StorageMap();
    if (config.procedureThresholds) {
      for (const pt of config.procedureThresholds) {
        const rootHex = getProcedureRoot(pt.procedure);
        const key = Word.fromHex(rootHex);
        const value = new Word(new BigUint64Array([BigInt(pt.threshold), 0n, 0n, 0n]));
        procThresholdMap.insert(key, value);
      }
    }
    const slot4 = StorageSlot.map(MULTISIG_SLOT_NAMES.PROCEDURE_THRESHOLDS, procThresholdMap);

    return [slot0, slot1, slot2, slot3, slot4];
  }

  buildGuardianSlots(config: MultisigConfig): StorageSlot[] {
    // The guarded-multisig has no enable/disable selector; the guardian is always present.
    // Only two slots: the guardian public key map and its scheme map.
    const schemeId = config.signatureScheme === 'ecdsa' ? 1n : 2n;
    const zeroKey = signerMapKey(0n);

    const guardianKeyMap = new StorageMap();
    const guardianKey = Word.fromHex(ensureHexPrefix(config.guardianCommitment));
    guardianKeyMap.insert(zeroKey, guardianKey);
    const slot0 = StorageSlot.map(GUARDIAN_SLOT_NAMES.PUBLIC_KEY, guardianKeyMap);

    const guardianSchemeMap = new StorageMap();
    const guardianScheme = new Word(new BigUint64Array([schemeId, 0n, 0n, 0n]));
    guardianSchemeMap.insert(zeroKey, guardianScheme);
    const slot1 = StorageSlot.map(GUARDIAN_SLOT_NAMES.SCHEME_ID, guardianSchemeMap);

    return [slot0, slot1];
  }
}

const defaultStorageBuilder = new StorageLayoutBuilder();

export function buildMultisigStorageSlots(config: MultisigConfig): StorageSlot[] {
  return defaultStorageBuilder.buildMultisigSlots(config);
}

export function buildGuardianStorageSlots(config: MultisigConfig): StorageSlot[] {
  return defaultStorageBuilder.buildGuardianSlots(config);
}

export const storageLayoutBuilder = defaultStorageBuilder;
