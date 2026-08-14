/**
 * Account Inspector - Inspects account storage to detect multisig configuration.
 */

import { Account, Word } from '@miden-sdk/miden-sdk';
import { base64ToUint8Array } from './utils/encoding.js';
import { isEmptyWord, wordElementToBigInt, wordToHex } from './utils/word.js';
import { getProcedureRoot, getProcedureNames, type ProcedureName } from './procedures.js';
import { MULTISIG_SLOT_NAMES, GUARDIAN_SLOT_NAMES, MAX_SIGNERS } from './account/layout.js';

type AccountStorageLike = ReturnType<Account['storage']>;

export interface VaultBalance {
  faucetId: string;
  amount: bigint;
}

export interface DetectedMultisigConfig {
  threshold: number;
  numSigners: number;
  signerCommitments: string[];
  guardianCommitment: string | null;
  vaultBalances: VaultBalance[];
  procedureThresholds: Map<ProcedureName, number>;
}

/**
 * Fail-closed validation for consuming a lenient `fromAccount()` result on a
 * mutation path. `fromAccount` tolerates partial reads (absent entries are
 * skipped), which is fine for inspection but not for callers that store the
 * result as the authoritative config: membership proposals treat the signer
 * set as the complete on-chain set, so adopting a truncated read could
 * rewrite the account without the omitted keys.
 */
export function assertCompleteDetectedConfig(
  detected: DetectedMultisigConfig,
): asserts detected is DetectedMultisigConfig & { guardianCommitment: string } {
  if (detected.numSigners === 0 || detected.signerCommitments.length !== detected.numSigners) {
    throw new Error(
      `incomplete signer set: storage reports ${detected.numSigners} signers, read ${detected.signerCommitments.length}`,
    );
  }
  if (!detected.guardianCommitment) {
    throw new Error(
      'missing guardian commitment: the guarded-multisig always includes a guardian',
    );
  }
}

/**
 * Rejects accounts built from a different contract version before any
 * layout-dependent read: against such an account the slot names and
 * procedure-root-keyed maps below describe a different component, so reads
 * would silently miss its state.
 */
function assertPinnedContractVersion(account: Account): void {
  if (!account.code().hasProcedure(Word.fromHex(getProcedureRoot('auth_tx')))) {
    throw new Error(
      'unsupported contract version: the account\'s code does not carry this ' +
      "SDK's pinned guarded-multisig auth procedure; use the SDK release " +
      'matching the contract version the account was created with ' +
      '(see docs/MULTISIG_SDK.md, "Contract version pinning")',
    );
  }
}

function indexMapKey(index: number): Word {
  return new Word(new BigUint64Array([BigInt(index), 0n, 0n, 0n]));
}

/**
 * Read one entry from a storage map, treating "no entry" uniformly.
 *
 * The SDK returns `undefined` only when the slot itself is absent or not a
 * map; a key with no entry in an existing map comes back as `Word::empty()`
 * (`StorageMap::get` is `unwrap_or_default()` in miden-protocol). The
 * contract also zeroes removed approver entries, so an empty word always
 * means "no entry" and is mapped to `undefined` here — matching the Rust
 * readers.
 */
function readMapWord(
  storage: AccountStorageLike,
  slotName: string,
  key: Word,
): Word | undefined {
  let value: Word | undefined;
  try {
    value = storage.getMapItem(slotName, key) as Word | undefined;
  } catch (error) {
    // The SDK's storage rejects Word instances constructed by a different
    // bundled copy of @miden-sdk/miden-sdk (wasm-bindgen instance check).
    if (error instanceof Error && error.message.includes('expected instance of Word')) {
      throw new Error(
        `cannot read ${slotName}: the account object comes from a different copy of @miden-sdk/miden-sdk than the one this package links; pass an Account created by the same SDK instance`,
        { cause: error },
      );
    }
    throw error;
  }
  if (value === undefined || isEmptyWord(value)) {
    return undefined;
  }
  return value;
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
   * Read the ordered approver (signer) public-key commitments of a
   * guarded-multisig account from its
   * `miden::standards::auth::multisig::approver_public_keys` storage map.
   *
   * Since the account uses the upstream `AuthGuardedMultisig` component,
   * `Account.getPublicKeyCommitments()` also returns these commitments;
   * this accessor is the strict, layout-insulated alternative: it validates
   * the complete set against the configured signer count and throws instead
   * of silently omitting unreadable entries, and it shields consumers from
   * storage-layout changes across contract versions.
   *
   * The returned array is ordered by signer index as currently stored.
   * Index 0 is the key listed first at creation only until the first
   * membership change: removing a signer re-packs the indices. Hot/cold
   * roles are a consumer-side convention, not part of on-chain state.
   *
   * Unlike `fromAccount()`, which tolerates partial reads, this throws if
   * the account was built from a different contract version or any signer
   * entry is absent — it never silently returns a truncated or empty list.
   *
   * The `account` must come from the same copy of `@miden-sdk/miden-sdk`
   * that this package links; an account from a separately bundled SDK is
   * rejected by the SDK's own instance checks (a descriptive error is
   * thrown).
   *
   * @param account - The Account object from the Miden SDK
   * @returns Signer public-key commitments as 0x-prefixed hex, ordered by signer index
   */
  static getSignerPublicKeyCommitments(account: Account): string[] {
    assertPinnedContractVersion(account);
    const storage = account.storage();

    const thresholdConfig = storage.getItem(MULTISIG_SLOT_NAMES.THRESHOLD_CONFIG) as
      | Word
      | undefined;
    if (!thresholdConfig) {
      throw new Error(
        `account has no ${MULTISIG_SLOT_NAMES.THRESHOLD_CONFIG} storage slot: not a guarded-multisig account`,
      );
    }

    const numSigners = Number(wordElementToBigInt(thresholdConfig, 1));
    if (numSigners === 0) {
      throw new Error(
        `${MULTISIG_SLOT_NAMES.THRESHOLD_CONFIG} reports zero signers: not a guarded-multisig account`,
      );
    }
    if (numSigners > MAX_SIGNERS) {
      throw new Error(
        `${MULTISIG_SLOT_NAMES.THRESHOLD_CONFIG} reports ${numSigners} signers, exceeding the sanity limit of ${MAX_SIGNERS}: account storage is corrupt`,
      );
    }

    const commitments: string[] = [];
    for (let i = 0; i < numSigners; i++) {
      const commitment = readMapWord(storage, MULTISIG_SLOT_NAMES.SIGNER_PUBLIC_KEYS, indexMapKey(i));
      if (!commitment) {
        throw new Error(
          `missing signer public key at index ${i} in ${MULTISIG_SLOT_NAMES.SIGNER_PUBLIC_KEYS} (expected ${numSigners} signers)`,
        );
      }
      commitments.push(wordToHex(commitment));
    }
    return commitments;
  }

  /**
   * Read the guardian public-key commitment of a guarded-multisig account
   * from its `miden::standards::auth::guardian::pub_key` storage map.
   *
   * The guarded-multisig component always includes a guardian, so this
   * returns the commitment or throws: on an account from a different
   * contract version, or when the guardian key entry is missing
   * (inconsistent account state). Genuine storage read failures propagate.
   *
   * @param account - The Account object from the Miden SDK
   * @returns The guardian commitment as 0x-prefixed hex
   */
  static getGuardianPublicKeyCommitment(account: Account): string {
    assertPinnedContractVersion(account);
    const storage = account.storage();

    const commitment = readMapWord(storage, GUARDIAN_SLOT_NAMES.PUBLIC_KEY, indexMapKey(0));
    if (!commitment) {
      throw new Error(
        `${GUARDIAN_SLOT_NAMES.PUBLIC_KEY} has no entry: inconsistent account state (the guarded-multisig always includes a guardian)`,
      );
    }
    return wordToHex(commitment);
  }

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
   * Lenient by design (skips unreadable parts): `MultisigClient.load` uses
   * it to reconstruct config from accounts it already trusts. Consumers that
   * need a guarantee should use `getSignerPublicKeyCommitments` /
   * `getGuardianPublicKeyCommitment`, which throw instead of degrading.
   *
   * @param account - The Account object from Miden SDK
   * @returns Detected multisig configuration
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  static fromAccount(account: Account): DetectedMultisigConfig {
    // Reject accounts built from a different contract version before any
    // procedure-root-keyed read: against such an account the reads below would
    // silently miss its stored overrides (its `procedure_thresholds` map is
    // keyed by *its* roots, not this SDK's) and report wrong thresholds.
    assertPinnedContractVersion(account);

    const storage = account.storage();

    const slot0 = storage.getItem(MULTISIG_SLOT_NAMES.THRESHOLD_CONFIG) as Word | undefined;
    const threshold = slot0 ? Number(wordElementToBigInt(slot0, 0)) : 0;
    const numSigners = slot0 ? Number(wordElementToBigInt(slot0, 1)) : 0;

    const signerCommitments: string[] = [];
    for (let i = 0; i < Math.min(numSigners, MAX_SIGNERS); i++) {
      try {
        const commitment = readMapWord(storage, MULTISIG_SLOT_NAMES.SIGNER_PUBLIC_KEYS, indexMapKey(i));
        if (commitment) {
          signerCommitments.push(wordToHex(commitment));
        }
      } catch (error) {
        console.warn(error);
      }
    }

    // The guarded-multisig has no enable/disable selector; the guardian is always present.
    // Read its public key directly from the guardian pub_key slot.
    let guardianCommitment: string | null = null;

    try {
      const guardianKey = readMapWord(storage, GUARDIAN_SLOT_NAMES.PUBLIC_KEY, indexMapKey(0));
      if (guardianKey) {
        guardianCommitment = wordToHex(guardianKey);
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

    // Read procedure threshold overrides from storage slot 3
    // Storage layout: slot 3 is a map of PROC_ROOT => [threshold, 0, 0, 0]
    const procedureThresholds = new Map<ProcedureName, number>();
    for (const procName of getProcedureNames()) {
      try {
        const rootHex = getProcedureRoot(procName);
        const rootWord = Word.fromHex(rootHex);
        const value = readMapWord(storage, MULTISIG_SLOT_NAMES.PROCEDURE_THRESHOLDS, rootWord);
        if (value) {
          const procThreshold = Number(wordElementToBigInt(value, 0));
          if (procThreshold > 0) {
            procedureThresholds.set(procName, procThreshold);
          }
        }
      } catch {
        // Procedure threshold not set - use default
      }
    }

    return {
      threshold,
      numSigners,
      signerCommitments,
      guardianCommitment,
      vaultBalances,
      procedureThresholds,
    };
  }
}
