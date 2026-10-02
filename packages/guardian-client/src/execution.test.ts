import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { describe, it, expect } from 'vitest';
import {
  EXECUTION_FAILURE_CODES,
  EXECUTION_STATES,
  EXECUTION_UNAVAILABLE_REASONS,
  EXPIRATION_BOUNDS,
  FOREIGN_ACCOUNT_UNAVAILABLE_REASONS,
  REQUEST_INVALID_REASONS,
  fromServerExecution,
  fromServerExecutionCapability,
  isTerminalExecutionState,
  type ServerProposalExecution,
} from './execution.js';

const executionRs = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), '../../../crates/shared/src/execution.rs'),
  'utf8'
);

function snake(variant: string): string {
  return variant.replace(/([a-z0-9])([A-Z])/g, '$1_$2').toLowerCase();
}

function rustVariants(enumName: string): string[] {
  const start = executionRs.indexOf(`pub enum ${enumName} {`);
  expect(start, `${enumName} is defined in guardian-shared`).toBeGreaterThan(-1);
  const body = executionRs.slice(start, executionRs.indexOf('}', start));
  return [...body.matchAll(/^\s+([A-Z][A-Za-z]+)[,(]/gm)].map((m) => m[1]);
}

function envelope(overrides: Partial<ServerProposalExecution>): ServerProposalExecution {
  return {
    account_id: '0xacc',
    proposal_id: '0xprop',
    state: 'pending',
    newly_accepted: false,
    proposal_exists: true,
    ignored_signatures: 0,
    updated_at: '2026-09-30T12:00:00Z',
    ...overrides,
  };
}

describe('drift guard against guardian_shared::execution', () => {
  it('the states and meta value sets match the Rust enums', () => {
    expect([...EXECUTION_STATES]).toEqual(rustVariants('ExecutionState').map(snake));
    expect([...REQUEST_INVALID_REASONS]).toEqual(rustVariants('RequestInvalidReason').map(snake));
    expect([...EXPIRATION_BOUNDS]).toEqual(rustVariants('ExpirationBound').map(snake));
    expect([...FOREIGN_ACCOUNT_UNAVAILABLE_REASONS]).toEqual(
      rustVariants('ForeignAccountUnavailableReason').map(snake)
    );
  });

  it('the execution-unavailable reasons match the server enum', () => {
    const configRs = readFileSync(
      join(dirname(fileURLToPath(import.meta.url)), '../../../crates/server/src/config/execution.rs'),
      'utf8'
    );
    const body = configRs.slice(configRs.indexOf('pub enum ExecutionUnavailable {'));
    const variants = [...body.slice(0, body.indexOf('\n}')).matchAll(/^\s{4}([A-Z][A-Za-z]+),$/gm)].map((m) => m[1]);
    expect([...EXECUTION_UNAVAILABLE_REASONS]).toEqual(variants.map(snake));
  });

  it('the failure codes match ExecutionFailureCode::as_str', () => {
    const start = executionRs.indexOf('pub fn as_str(&self) -> &\'static str {', executionRs.indexOf('impl ExecutionFailureCode'));
    const body = executionRs.slice(start, executionRs.indexOf('pub fn meta', start));
    const rustCodes = [...body.matchAll(/"(GUARDIAN_EXECUTION_[A-Z_]+)"/g)].map((m) => m[1]);
    expect([...EXECUTION_FAILURE_CODES].sort()).toEqual(rustCodes.sort());
  });

  it('never carries the codes the design withdrew', () => {
    for (const withdrawn of ['ANCHOR_EXPIRED', 'NO_FINITE_EXPIRATION', 'FOREIGN_INPUTS_UNSUPPORTED']) {
      expect(EXECUTION_FAILURE_CODES.some((code) => code.endsWith(withdrawn))).toBe(false);
    }
  });
});

describe('fromServerExecution', () => {
  it('decodes a failure with its typed meta', () => {
    const execution = fromServerExecution(
      envelope({
        state: 'failed',
        proposal_exists: false,
        error: {
          code: 'GUARDIAN_EXECUTION_EXPIRATION_REACHED',
          message: 'expired',
          meta: { bound: 'transaction' },
        },
      })
    );
    expect(execution.error).toEqual({
      code: 'GUARDIAN_EXECUTION_EXPIRATION_REACHED',
      message: 'expired',
      bound: 'transaction',
    });
    expect(execution.proposalExists).toBe(false);
  });

  it('refuses an unknown state, an unknown reason and a failure without its error', () => {
    expect(() => fromServerExecution(envelope({ state: 'queued' }))).toThrow();
    expect(() =>
      fromServerExecution(
        envelope({
          state: 'failed',
          error: { code: 'GUARDIAN_EXECUTION_REQUEST_INVALID', message: 'x', meta: { reason: 'other' } },
        })
      )
    ).toThrow();
    expect(() => fromServerExecution(envelope({ state: 'failed' }))).toThrow();
  });

  it('maps nothing in flight and an absent nonce to null', () => {
    expect(fromServerExecution(envelope({})).deltaNonce).toBeNull();
    expect(fromServerExecution(envelope({ delta_nonce: 4 })).deltaNonce).toBe(4);
  });
});

describe('isTerminalExecutionState', () => {
  it('matches ExecutionState::is_terminal', () => {
    expect(EXECUTION_STATES.filter(isTerminalExecutionState)).toEqual(['committed', 'failed']);
  });
});

describe('fromServerExecutionCapability', () => {
  it('decodes both shapes', () => {
    expect(fromServerExecutionCapability({ enabled: true })).toEqual({ enabled: true });
    expect(fromServerExecutionCapability({ enabled: false, reason: 'disabled' })).toEqual({
      enabled: false,
      reason: 'disabled',
    });
  });

  it('refuses an unknown reason, a missing field and a reason on an enabled capability', () => {
    expect(() => fromServerExecutionCapability({ enabled: false, reason: 'gone_fishing' })).toThrow(
      /unknown execution unavailable reason/
    );
    expect(() => fromServerExecutionCapability({ enabled: false })).toThrow(/unknown execution unavailable reason/);
    expect(() => fromServerExecutionCapability({ enabled: 'yes' })).toThrow(/boolean enabled/);
    expect(() => fromServerExecutionCapability(undefined)).toThrow(/no execution capability/);
    expect(() => fromServerExecutionCapability({ enabled: true, reason: 'disabled' })).toThrow(/with a reason/);
  });
});
