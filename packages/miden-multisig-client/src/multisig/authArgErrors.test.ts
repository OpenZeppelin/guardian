import { describe, expect, it } from 'vitest';

import {
  ProposalAuthArgUnresolvableError,
  ProposalSaltMalformedError,
} from './authArgErrors.js';

/**
 * These two errors are the ones in this package a caller is expected to branch on
 * rather than report: both mean the proposal itself is dead, as against a
 * transport or WASM failure that a retry might clear. A renamed code or a dropped
 * field would silently break that branch, so the identifying surface is pinned
 * here rather than left to the call site's tests.
 */
describe('ProposalAuthArgUnresolvableError', () => {
  const error = new ProposalAuthArgUnresolvableError({
    proposalId: '0xaaaa',
    signedAuthArgHex: '0xf00d',
    saltHex: '0xbeef',
    feeFaucetIdHex: '0xcafe',
  });

  it('carries a stable code and name', () => {
    expect(error.code).toBe('proposal_auth_arg_unresolvable');
    expect(error.name).toBe('ProposalAuthArgUnresolvableError');
    expect(error).toBeInstanceOf(Error);
  });

  it('keeps every value needed to diagnose the mismatch structured', () => {
    expect(error.proposalId).toBe('0xaaaa');
    expect(error.signedAuthArgHex).toBe('0xf00d');
    expect(error.saltHex).toBe('0xbeef');
    expect(error.feeFaucetIdHex).toBe('0xcafe');
  });

  it('names all three operands in the message', () => {
    expect(error.message).toContain('0xf00d');
    expect(error.message).toContain('0xbeef');
    expect(error.message).toContain('0xcafe');
  });
});

/** Quoted salt cap, plus room for the fixed prose and the reason. */
const MAX_MESSAGE_OVERHEAD = 200;

describe('ProposalSaltMalformedError', () => {
  it('carries a stable code and name distinct from the unresolvable case', () => {
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0xnope',
      reason: 'expected a 32-byte hex word',
    });

    expect(error.code).toBe('proposal_salt_malformed');
    expect(error.name).toBe('ProposalSaltMalformedError');
    expect(error.proposalId).toBe('0xaaaa');
    expect(error.saltHex).toBe('0xnope');
    expect(error.message).toContain("'0xnope': expected a 32-byte hex word");
  });

  /**
   * The salt is served as JSON and the response is cast, not validated, so this
   * error must be constructible from any value at all — a throwing `toString`
   * would otherwise replace it with an uncoded `TypeError`.
   */
  it('describes a value whose own toString throws', () => {
    const hostile = {
      toString() {
        throw new Error('nope');
      },
    };
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: hostile,
      reason: 'expected a hex string, got object',
    });

    expect(error.code).toBe('proposal_salt_malformed');
    expect(error.message).toContain('<undescribable object>');
    expect(error.saltHex).toBe(hostile);
  });

  /**
   * Truncating the message is pointless if the default log path prints the raw
   * value anyway, which it does for any own enumerable property.
   */
  it('keeps the raw salt out of inspect and JSON output', () => {
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0x' + 'a'.repeat(5000),
      reason: 'expected a 32-byte hex word',
    });

    expect(Object.keys(error)).not.toContain('saltHex');
    expect(JSON.stringify(error)).not.toContain('aaaaaaaaaa');
    expect(error.saltHex).toHaveLength(5002);
    expect(error.message).toContain('5002 code units');
  });

  it('preserves the underlying decode failure as a cause', () => {
    const cause = new Error('value >= field modulus');
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0x' + 'f'.repeat(64),
      reason: 'it is not a readable field element',
      cause,
    });

    expect(error.cause).toBe(cause);
  });

  /**
   * The salt is attacker-chosen metadata, and this message reaches a log. An
   * unbounded value would let a GUARDIAN pad a log line at will, and raw control
   * bytes would reach the terminal verbatim.
   */
  it('truncates an over-long salt and strips control characters', () => {
    // A realistic 66-char proposal id, so the bound is about the salt rather
    // than about a short fixture.
    const proposalId = '0x' + 'c'.repeat(64);
    const error = new ProposalSaltMalformedError({
      proposalId,
      saltHex: '0x' + 'a'.repeat(5000),
      reason: 'expected a 32-byte hex word',
    });

    expect(error.message.length).toBeLessThan(proposalId.length + MAX_MESSAGE_OVERHEAD);
    expect(error.message).toContain('5002 code units');
    expect(error.saltHex).toHaveLength(5002);

    const escaped = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0x\u001b[31mred\n',
      reason: 'expected a 32-byte hex word',
    });

    expect(escaped.message).not.toMatch(/[\u001b\n]/);
    expect(escaped.message).toContain('0x.[31mred.');
  });

  /**
   * Control characters past the truncation point still have to be gone: the
   * cheap way to avoid copying an oversized salt is to slice before stripping,
   * which is only safe if the slice is what gets stripped.
   */
  it('strips control characters that survive into the truncated prefix', () => {
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '\u001b[31m' + 'a'.repeat(5000),
      reason: 'expected a 32-byte hex word',
    });

    expect(error.message).not.toMatch(/[\u001b\n]/);
    expect(error.message).toContain('.[31m');
  });
});
