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
  update_signers: '0x29d26091f4e8a13727e7937a62ad412a392e984eaa1fcce93439a95ab5e003ee',
  update_procedure_threshold: '0xcda1f9120a3ab2948d5cdc6b4b2982571c04e3f6af787a6d6b2f88eeedd872d7',
  auth_tx: '0x611abcd570631ad98842cb6f0ef891fe8f9ee508b3245c55a5531d5a8f7fdca9',
  update_psm: '0x35498ce6e3bc24ae0e0094dc54a09b8b2bbcbc28607f86ba25684cd4a2d8f55b',
  verify_psm: '0x2f1b90e9d89f1a541dd8621444edba9d3e0a66ef54147ebf59bf964969b9dfd1',
  send_asset: '0x0e406b067ed2bcd7de745ca6517f519fd1a9be245f913347ac673ca1db30c1d6',
  receive_asset: '0x6f4bdbdc4b13d7ed933d590d88ac9dfb98020c9e917697845b5e169395b76a01',
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
 * // '0x0e406b067ed2bcd7de745ca6517f519fd1a9be245f913347ac673ca1db30c1d6'
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
