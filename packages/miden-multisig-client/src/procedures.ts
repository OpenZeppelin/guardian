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
  update_signers: '0x4317e9e5026f51d0eb0fa45a38e24b4c974e136b3f031705307dc1a595caa83e',
  update_procedure_threshold: '0xec74c4b96ce593c11017ae54dec9c0ae5e0d242e8b3074eb3908d961300aed67',
  auth_tx: '0x9926033d18d5cbb93367002d429bdc869d0156ecb67d53e8b033c99b983b1c22',
  update_guardian: '0xeceb1f2c2d7d20312dbaf091e9a27a2b63f9fcba120948043069793a5715bc96',
  verify_guardian: '0x6a3105ef57d14ca460bb013a8887e626e92476090fc09b56cd370eff39dad8f5',
  send_asset: '0x6f6abe6b4cc278b411af4abb43754a490b7361f85acbabec8ee6e109908a9340',
  receive_asset: '0x34a56dd18f6fe5aab63198b9dcfc6467e793ebabb37d56b994b902504635da13',
  create_note: '0xa185681459a0a2bbb32f9982e5bb764a52f8cfdfd74c777d2002104d2a25b931',
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
