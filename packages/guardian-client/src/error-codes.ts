/**
 * Typed vocabulary of Guardian's stable, machine-readable error codes
 * (issue #318). Mirrors `GuardianError::code()` in
 * `crates/server/src/error.rs` — a drift-guard test asserts the two stay in
 * sync. The four codes the server emits in SCREAMING_SNAKE wire form
 * (`GUARDIAN_ACCOUNT_PAUSED`, `GUARDIAN_ACCOUNT_RELEASED`,
 * `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`, `GUARDIAN_CANDIDATE_LANDED`)
 * are normalized to snake_case here so the union has one consistent shape; use
 * {@link normalizeGuardianErrorCode} at the wire boundary.
 */
export const GUARDIAN_ERROR_CODES = [
  'account_already_exists',
  'account_data_unavailable',
  'account_not_found',
  'account_paused',
  'account_released',
  'authentication_failed',
  'authorization_failed',
  'candidate_landed',
  'commitment_mismatch',
  'configuration_error',
  'conflict_pending_delta',
  'conflict_pending_proposal',
  'data_unavailable',
  'delta_not_found',
  'insufficient_operator_permission',
  'insufficient_signatures',
  'invalid_account_id',
  'invalid_commitment',
  'invalid_cursor',
  'invalid_delta',
  'invalid_evm_proposal',
  'invalid_input',
  'invalid_limit',
  'invalid_network_config',
  'invalid_proposal_signature',
  'invalid_status_filter',
  'network_error',
  'pending_proposals_limit',
  'proposal_already_signed',
  'proposal_not_found',
  'rate_limit_exceeded',
  'rpc_unavailable',
  'rpc_validation_failed',
  'signer_not_authorized',
  'signing_error',
  'state_not_found',
  'storage_error',
  'unsupported_evm_chain',
  'unsupported_for_network',
] as const;

/**
 * A stable Guardian error code, compiler-checked: comparing against a
 * non-member literal is a type error, and `switch` statements can be
 * exhaustiveness-checked. Branch and localize on this — never on message
 * text or the HTTP status alone.
 */
export type GuardianErrorCode = (typeof GUARDIAN_ERROR_CODES)[number];

const CODE_SET: ReadonlySet<string> = new Set(GUARDIAN_ERROR_CODES);

/**
 * Wire form → union form for the codes the server emits in SCREAMING_SNAKE.
 * Every other code is already snake_case on the wire.
 */
const WIRE_ALIASES: Readonly<Record<string, GuardianErrorCode>> = {
  GUARDIAN_ACCOUNT_PAUSED: 'account_paused',
  GUARDIAN_ACCOUNT_RELEASED: 'account_released',
  GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION: 'insufficient_operator_permission',
  GUARDIAN_CANDIDATE_LANDED: 'candidate_landed',
};

/** Type guard narrowing an arbitrary string to {@link GuardianErrorCode}. */
export function isGuardianErrorCode(value: string): value is GuardianErrorCode {
  return CODE_SET.has(value);
}

/**
 * Normalize a server-emitted wire code to the typed union: SCREAMING_SNAKE
 * aliases map to their snake_case form; recognized snake_case codes pass
 * through. Returns `null` for codes outside the known vocabulary (e.g. a
 * newer server) — callers keep the verbatim wire string separately (see
 * `GuardianHttpError.rawCode`) rather than letting an unknown code silently
 * satisfy a string branch.
 */
export function normalizeGuardianErrorCode(raw: string): GuardianErrorCode | null {
  const aliased = WIRE_ALIASES[raw];
  if (aliased !== undefined) return aliased;
  return isGuardianErrorCode(raw) ? raw : null;
}
