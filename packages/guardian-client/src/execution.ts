/**
 * Guardian execution of threshold-met proposals, as the base client reports it. Mirrors the
 * `guardian_shared::execution` vocabulary; drift-guard tests keep the two in step.
 */

/** The five reported states. Closed: adding one is a breaking change. */
export const EXECUTION_STATES = ['pending', 'proving', 'submitted', 'committed', 'failed'] as const;
export type ExecutionState = (typeof EXECUTION_STATES)[number];

/** Whether the execution has finished, one way or the other. Mirrors `ExecutionState::is_terminal`. */
export function isTerminalExecutionState(state: ExecutionState): boolean {
  switch (state) {
    case 'committed':
    case 'failed':
      return true;
    case 'pending':
    case 'proving':
    case 'submitted':
      return false;
    default: {
      const unreachable: never = state;
      throw new Error(`Unknown execution state: ${String(unreachable)}`);
    }
  }
}

export const REQUEST_INVALID_REASONS = [
  'bound_block_not_declared',
  'auth_args_missing',
  'approval_expiration_missing',
  'input_notes_not_pinned',
] as const;
export type RequestInvalidReason = (typeof REQUEST_INVALID_REASONS)[number];

export const EXPIRATION_BOUNDS = ['approval', 'transaction'] as const;
export type ExpirationBound = (typeof EXPIRATION_BOUNDS)[number];

export const FOREIGN_ACCOUNT_UNAVAILABLE_REASONS = ['private', 'unavailable'] as const;
export type ForeignAccountUnavailableReason = (typeof FOREIGN_ACCOUNT_UNAVAILABLE_REASONS)[number];

/** Why a server does not offer Guardian execution, as `GET /status` reports it. */
export const EXECUTION_UNAVAILABLE_REASONS = ['prover_not_configured', 'disabled', 'canonicalization_disabled'] as const;
export type ExecutionUnavailableReason = (typeof EXECUTION_UNAVAILABLE_REASONS)[number];

/**
 * Whether the server accepts Guardian execution requests. It reflects the server's configuration,
 * not whether its prover is reachable right now.
 */
export type ServerExecutionCapability = { enabled: true } | { enabled: false; reason: ExecutionUnavailableReason };

/** The capability as `GET /status` sends it, before validation. */
export interface ServerExecutionCapabilityWire {
  enabled: unknown;
  reason?: unknown;
}

/** Failure codes that carry no structured meta. */
export const PLAIN_EXECUTION_FAILURE_CODES = [
  'GUARDIAN_EXECUTION_BINDING_MISMATCH',
  'GUARDIAN_EXECUTION_STATE_MISMATCH',
  'GUARDIAN_EXECUTION_REQUEST_CODEC',
  'GUARDIAN_EXECUTION_PROTOCOL_MISMATCH',
  'GUARDIAN_EXECUTION_CHAIN_BEHIND',
  'GUARDIAN_EXECUTION_CHAIN_INCONSISTENT',
  'GUARDIAN_EXECUTION_NODE_UNAVAILABLE',
  'GUARDIAN_EXECUTION_INSUFFICIENT_FEE',
  'GUARDIAN_EXECUTION_INSUFFICIENT_SIGNATURES',
  'GUARDIAN_EXECUTION_PROVING_FAILED',
  'GUARDIAN_EXECUTION_SEALING_FAILED',
  'GUARDIAN_EXECUTION_ACKNOWLEDGEMENT_FAILED',
  'GUARDIAN_EXECUTION_EXPIRATION_BEYOND_HORIZON',
  'GUARDIAN_EXECUTION_ACCOUNT_INADMISSIBLE',
  'GUARDIAN_EXECUTION_SUBMISSION_REJECTED',
  'GUARDIAN_EXECUTION_CANDIDATE_DISCARDED',
  'GUARDIAN_EXECUTION_EXPIRED',
  'GUARDIAN_EXECUTION_LEASE_EXPIRED',
  'GUARDIAN_EXECUTION_ABANDONED',
] as const;
export type PlainExecutionFailureCode = (typeof PLAIN_EXECUTION_FAILURE_CODES)[number];

/** Why an execution failed, discriminated by `code`; codes with meta carry it typed. */
export type ExecutionFailure =
  | { code: 'GUARDIAN_EXECUTION_REQUEST_INVALID'; message: string; reason: RequestInvalidReason }
  | { code: 'GUARDIAN_EXECUTION_EXPIRATION_REACHED'; message: string; bound: ExpirationBound }
  | {
      code: 'GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE';
      message: string;
      reason: ForeignAccountUnavailableReason;
    }
  | { code: PlainExecutionFailureCode; message: string };

export type ExecutionFailureCode = ExecutionFailure['code'];

export const EXECUTION_FAILURE_CODES: readonly ExecutionFailureCode[] = [
  ...PLAIN_EXECUTION_FAILURE_CODES,
  'GUARDIAN_EXECUTION_REQUEST_INVALID',
  'GUARDIAN_EXECUTION_EXPIRATION_REACHED',
  'GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE',
];

/** One execution of a proposal, identified by `(accountId, proposalId)`. */
export interface ProposalExecution {
  accountId: string;
  proposalId: string;
  state: ExecutionState;
  /** Present exactly when `state` is `'failed'`. */
  error: ExecutionFailure | null;
  /** The delta nonce the execution committed to, once it crossed the no-retry boundary. */
  deltaNonce: number | null;
  /** Whether this request started the execution, rather than finding it already running. */
  newlyAccepted: boolean;
  /** Whether the proposal is still stored. A fact, not retry advice. */
  proposalExists: boolean;
  /** Stored signatures the server ignored as invalid, duplicate or not from a cosigner. */
  ignoredSignatures: number;
  updatedAt: string;
}

export interface ServerExecutionError {
  code: string;
  message: string;
  meta?: Record<string, unknown>;
}

export interface ServerProposalExecution {
  account_id: string;
  proposal_id: string;
  state: string;
  error?: ServerExecutionError;
  delta_nonce?: number;
  newly_accepted: boolean;
  proposal_exists: boolean;
  ignored_signatures: number;
  updated_at: string;
}

export interface ServerCurrentExecution {
  execution: ServerProposalExecution | null;
}

function member<T extends string>(values: readonly T[], value: unknown, what: string): T {
  if (typeof value === 'string' && (values as readonly string[]).includes(value)) {
    return value as T;
  }
  throw new Error(`Guardian returned an unknown ${what}: ${JSON.stringify(value)}`);
}

/** Decodes the `GET /status` capability, refusing a missing flag or an unknown reason. */
export function fromServerExecutionCapability(server: ServerExecutionCapabilityWire | undefined): ServerExecutionCapability {
  if (typeof server !== 'object' || server === null) {
    throw new Error(`Guardian returned no execution capability: ${JSON.stringify(server)}`);
  }
  if (server.enabled === true) {
    if (server.reason !== undefined) {
      throw new Error(`Guardian returned an enabled execution capability with a reason: ${JSON.stringify(server.reason)}`);
    }
    return { enabled: true };
  }
  if (server.enabled === false) {
    return { enabled: false, reason: member(EXECUTION_UNAVAILABLE_REASONS, server.reason, 'execution unavailable reason') };
  }
  throw new Error(`Guardian returned an execution capability without a boolean enabled: ${JSON.stringify(server.enabled)}`);
}

function fromServerFailure(error: ServerExecutionError): ExecutionFailure {
  const { code, message, meta } = error;
  switch (code) {
    case 'GUARDIAN_EXECUTION_REQUEST_INVALID':
      return { code, message, reason: member(REQUEST_INVALID_REASONS, meta?.reason, 'request reason') };
    case 'GUARDIAN_EXECUTION_EXPIRATION_REACHED':
      return { code, message, bound: member(EXPIRATION_BOUNDS, meta?.bound, 'expiration bound') };
    case 'GUARDIAN_EXECUTION_FOREIGN_ACCOUNT_UNAVAILABLE':
      return {
        code,
        message,
        reason: member(FOREIGN_ACCOUNT_UNAVAILABLE_REASONS, meta?.reason, 'foreign account reason'),
      };
    default:
      return { code: member(PLAIN_EXECUTION_FAILURE_CODES, code, 'execution failure code'), message };
  }
}

/** Decodes the wire envelope, refusing any state, code or meta outside the vocabulary. */
export function fromServerExecution(server: ServerProposalExecution): ProposalExecution {
  const state = member(EXECUTION_STATES, server.state, 'execution state');
  const error = server.error ? fromServerFailure(server.error) : null;
  if ((state === 'failed') !== (error !== null)) {
    throw new Error(`Guardian returned an execution in state '${state}' ${error ? 'with' : 'without'} an error`);
  }
  return {
    accountId: server.account_id,
    proposalId: server.proposal_id,
    state,
    error,
    deltaNonce: server.delta_nonce ?? null,
    newlyAccepted: server.newly_accepted,
    proposalExists: server.proposal_exists,
    ignoredSignatures: server.ignored_signatures,
    updatedAt: server.updated_at,
  };
}
