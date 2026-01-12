/**
 * known procedure roots for multisig accounts.
 * Extracted from: cargo test --package miden-confidential-contracts log_procedure_roots -- --nocapture
 */

/**
 * Static mapping of procedure names to their deterministic roots.
 *
 * Component ordering: Multisig (auth) -> PSM -> BasicWallet
 */
export const PROCEDURE_ROOTS = {
  // Multisig component procedures
  /** Update signer list and threshold configuration */
  update_signers: '0x26905086c572765c44337002a961a4d69514889d7e55686dc31c00383b614c47',
  /** Authenticate transaction with multisig (Falcon512) */
  auth_tx: '0x2bc7664a9dd47b36e7c8b8c3df03412798e4410173f36acfe03d191a38add053',

  // PSM component procedures
  /** Update PSM public key */
  update_psm: '0x26ec27195f1fd3eb622b851dfc9eab038bca87522cfc7ec209bfe507682303b1',
  /** Verify PSM signature */
  verify_psm: '0x878a1f70568f2c3798cfa0163fc085fa350f92c1b4a8fe78a605613cc27f7230',

  // BasicWallet procedures (from miden_lib)
  /** Send assets from account (move_asset_to_note) */
  send_asset: '0xd6c130dba13c67ac4733915f24bea9d19f517f51a65c74ded7bcd27e066b400e',
  /** Receive assets into account */
  receive_asset: '0x016ab79593165e5b849776919e0c0298fb9dac880d593d93edd7134bdcdb4b6f',
} as const;

/**
 * Valid procedure names that can be used for threshold overrides.
 */
export type ProcedureName = keyof typeof PROCEDURE_ROOTS;

/**
 * Get the procedure root for a given procedure name.
 *
 * @param name - The procedure name
 * @returns The procedure root as a hex string
 *
 * @example
 * ```typescript
 * const root = getProcedureRoot('receive_asset');
 * // '0x016ab79593165e5b849776919e0c0298fb9dac880d593d93edd7134bdcdb4b6f'
 * ```
 */
export function getProcedureRoot(name: ProcedureName): string {
  return PROCEDURE_ROOTS[name];
}

/**
 * Check if a string is a valid procedure name.
 *
 * @param name - The string to check
 * @returns true if the string is a valid procedure name
 */
export function isProcedureName(name: string): name is ProcedureName {
  return name in PROCEDURE_ROOTS;
}

/**
 * Get all available procedure names.
 *
 * @returns Array of all valid procedure names
 */
export function getProcedureNames(): ProcedureName[] {
  return Object.keys(PROCEDURE_ROOTS) as ProcedureName[];
}
