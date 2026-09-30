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
  update_signers: '0xe39b380d435dd42206fd625fcddfd26a379a2312cfef0271e9b5f18cbdec67e5',
  update_procedure_threshold: '0x5de3563f30c5dd130da49c8fdd86d867fed6dbbd928363021c53ef99d8034bac',
  auth_tx: '0xf988ff88c7a9c2104d77862d580239ec40e060a9b4d2d96028135abeb8cc58bc',
  update_guardian: '0x93dedb135fd5bb7112c07aacf4a5680ddc76e45ecf043bfc42ea735b6d971911',
  send_asset: '0x936e9920bffd7f458cc9ba2c4bbcc018fc4d3561511d79635955129268039dd7',
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
 * // '0x936e9920bffd7f458cc9ba2c4bbcc018fc4d3561511d79635955129268039dd7'
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
