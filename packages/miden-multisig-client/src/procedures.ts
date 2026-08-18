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
  update_signers: '0xa261cfd3c8791ac5abe1e78e14eade2f20789d73ab1c23c430418de59bc3380e',
  update_procedure_threshold: '0x97587c61d49313b1d5a3c8b7437e0080e67ed9bd9d3e7206bcae562f934ccd03',
  auth_tx: '0xa6aa6f69d9358535272ba433cd48d20628a5c69598e00c6dd01a22e83a5f15df',
  update_guardian: '0x0a614ff7c81a561cbd2a4c2d9482031a7a841ca5de33349daed23a9d871b3675',
  send_asset: '0x595bc83258726a66bd904912cfd5186c07cbd902dfbc115b7d6bc8105efc57e3',
  receive_asset: '0x34a56dd18f6fe5aab63198b9dcfc6467e793ebabb37d56b994b902504635da13',
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
