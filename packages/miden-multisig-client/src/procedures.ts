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
  update_signers: '0x34963b067dbba634e57b416bc2f2a9a8d4ac24147f40b2900148c9ba44774274',
  update_procedure_threshold: '0xec74c4b96ce593c11017ae54dec9c0ae5e0d242e8b3074eb3908d961300aed67',
  auth_tx: '0x841bba7204d80cd5f704da202fa54e968455547e37a926ff111dae4241f34d05',
  update_guardian: '0xeceb1f2c2d7d20312dbaf091e9a27a2b63f9fcba120948043069793a5715bc96',
  verify_guardian: '0xe6a8a62d37117f55a79b5345aa3d263ab16e973d486bac9a1612663dfdecf82d',
  send_asset: '0xfb1c73d10de1954e9e8948964e3e77cf4e33759d2e012cb00eb10c50f2974eb4',
  receive_asset: '0x6170fd6d682d91777b551fd866258f43cc657f1291f8f071500f4e56e9c153da',
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
