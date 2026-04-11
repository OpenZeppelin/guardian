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
  update_signers: '0x3d382ad461f9914c487c6fe908991d088eb54ecbd4aa8560ef79c66c3746bf19',
  update_procedure_threshold: '0x1f43e9d56ceff5d547ffdcb89896fb38cae0be1b74d9235ed2b4aa525df85f8d',
  auth_tx: '0x415530d7169f849d7219e810065f9119bba9af2c55070de0bf4f082a1c0aea5c',
  update_guardian: '0xc8ea876f1837e5cd1d6031becdbd40ce262ecd55930d65400f6890a37149d80c',
  verify_guardian: '0x9bc6e7b25c8dbaa29d6ad41e354a545dd0a4bac7f3a521bb5195ba101f0213cc',
  send_asset: '0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89',
  receive_asset: '0x75f638c65584d058542bcf4674b066ae394183021bc9b44dc2fdd97d52f9bcfb',
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
 * // '0x6d30df4312a2c44ec842db1bee227cc045396ca91e2c47d756dcb607f2bf5f89'
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
