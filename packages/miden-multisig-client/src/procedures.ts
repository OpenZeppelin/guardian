/**
 * known procedure roots for multisig accounts.
 *
 * IMPORTANT: These values are extracted from the TypeScript SDK's compiled MASM output,
 * NOT from the Rust crate. The SDK may compile MASM differently, resulting in different
 * procedure roots. Use the debug output from builder.ts to update these values.
 */

/**
 * Static mapping of procedure names to their deterministic roots.
 *
 * Component ordering: Multisig (auth) -> PSM -> BasicWallet
 */
export const PROCEDURE_ROOTS = {
  // Multisig component procedures
  /** Update signer list and threshold configuration */
  update_signers: '0x53d0ad381a193de0cf6af3730141e498274103dfc3b8c8e7367bd49d4a66c72b',
  /** Authenticate transaction with multisig (Falcon512) */
  auth_tx: '0x474c613b38001cc36d68557e9d881495d6a461a9027033445c7672c586509026',

  // PSM component procedures
  /** Update PSM public key */
  update_psm: '0xb103236807e5bf09c27efc2c5287ca8b03ab9efc1d852b62ebd31f5f1927ec26',
  /** Verify PSM signature */
  verify_psm: '0x30727fc23c6105a678fea8b4c1920f35fa85c03f16a0cf98372c8f56701f8a87',

  // BasicWallet procedures
  /** Send assets from account (move_asset_to_note) */
  send_asset: '0x0e406b067ed2bcd7de745ca6517f519fd1a9be245f913347ac673ca1db30c1d6',
  /** Receive assets into account */
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
 * @returns The procedure root as a hex string
 *
 * @example
 * ```typescript
 * const root = getProcedureRoot('receive_asset');
 * // '0x6f4bdbdc4b13d7ed933d590d88ac9dfb98020c9e917697845b5e169395b76a01'
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
