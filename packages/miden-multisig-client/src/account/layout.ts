/**
 * `AuthGuardedMultisig` storage slot names (`miden::standards::auth::*`),
 * shared by the writer (`account/storage.ts`) and the readers
 * (`inspector.ts`). Single source of truth so the two cannot drift. These
 * must match the Rust `miden-standards` component exactly: account ID and
 * commitment derive from the storage layout, so any divergence breaks
 * cross-SDK determinism (guarded by the parity test).
 *
 * Deliberately not re-exported from the package index: consumers should use
 * the `AccountInspector` accessors rather than reading storage directly
 * (issue #306).
 */

export const MULTISIG_SLOT_NAMES = {
  THRESHOLD_CONFIG: 'miden::standards::auth::multisig::threshold_config',
  SIGNER_PUBLIC_KEYS: 'miden::standards::auth::multisig::approver_public_keys',
  SIGNER_SCHEME_IDS: 'miden::standards::auth::multisig::approver_schemes',
  EXECUTED_TRANSACTIONS: 'miden::standards::auth::multisig::executed_transactions',
  PROCEDURE_THRESHOLDS: 'miden::standards::auth::multisig::procedure_thresholds',
} as const;

export const GUARDIAN_SLOT_NAMES = {
  PUBLIC_KEY: 'miden::standards::auth::guardian::pub_key',
  SCHEME_ID: 'miden::standards::auth::guardian::scheme',
} as const;

/**
 * Sanity ceiling for the signer count read from `threshold_config`. The
 * contract has no on-chain maximum (the felt is only asserted to be a u32),
 * so a corrupt or adversarial account could report an arbitrarily large
 * count; readers bound their loops with this instead of trusting it.
 */
export const MAX_SIGNERS = 1000;
