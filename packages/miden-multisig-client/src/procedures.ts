/**
 * Static mapping of procedure names to their deterministic roots.
 *
 * These values use the Miden SDK `Word.toHex()` / `Word.fromHex()` encoding, which is the
 * representation used by the TypeScript client when writing and reading storage map keys.
 *
 * Source of truth:
 * `cargo run --quiet --example procedure_roots -p miden-multisig-client -- --json`
 *
 * Note: the Rust example also prints `rust_hex` values for `procedures.rs`. Those are a different
 * human-readable encoding and should not be copied into this table.
 */
export const PROCEDURE_ROOTS = {
  update_signers: '0x0f664cdaae422fe43bb45d959c7e469b0c855ad1fc4e79fddd730ec3cb983c6e',
  update_procedure_threshold: '0xa32cd13808fd8fb91adb3dceaed4d8faebc3bd80fa49c2903be4b8d7bf89ba77',
  auth_tx: '0x71ba7380c6138d5e80e911094a9767ed0fa5c8ffefcf6a776754bfc75bc163b9',
  update_guardian: '0x93dedb135fd5bb7112c07aacf4a5680ddc76e45ecf043bfc42ea735b6d971911',
  send_asset: '0xf261e7bdd1faee5db3b0abe4bb67b153fbca6ece3e456b83eff98697f21f6a97',
  receive_asset: '0xd7416b798a70aabbca510c3cd0f48ba35473b5d76dc302375157c6f563fffc15',
} as const;

/**
 * Valid procedure names that can be used for threshold overrides.
 */
export type ProcedureName = keyof typeof PROCEDURE_ROOTS;

/**
 * Get the procedure root for a given procedure name.
 *
 * @param name - The procedure name
 * @returns The procedure root as a hex string in SDK `Word.toHex()` format
 *
 * @example
 * ```typescript
 * const root = getProcedureRoot('send_asset');
 * // '0xf261e7bdd1faee5db3b0abe4bb67b153fbca6ece3e456b83eff98697f21f6a97'
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
