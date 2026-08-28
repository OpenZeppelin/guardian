import { describe, expect, it } from 'vitest';

import {
  describeWithCause,
  ProposalAuthArgUnresolvableError,
  ProposalSaltMalformedError,
} from './authArgErrors.js';

/**
 * These two errors are the only ones in this package a caller is expected to
 * branch on rather than report: `switch_guardian` recovery routes around exactly
 * this pair and must keep propagating everything else. A renamed code or a
 * dropped field would silently turn that recovery into a rethrow, so the
 * identifying surface is pinned here rather than left to the call site's tests.
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
   * error must be constructible from any value at all — it is the one a
   * `switch_guardian` recovers from, and a throwing `toString` would replace it
   * with an uncoded `TypeError` that strands the switch.
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
    expect(error.quotedSalt).toContain('5002 code units');
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

/**
 * The reason a salt failed to decode lives only on the cause, and the fallback
 * warning is the one place a caller sees it. Returning just `error.message`
 * there loses it, which is what this pins.
 */
describe('describeWithCause', () => {
  it('appends the cause when there is one', () => {
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0x' + 'f'.repeat(64),
      reason: 'it is not a readable field element',
      cause: new Error('value >= field modulus'),
    });

    expect(describeWithCause(error)).toBe(`${error.message}: value >= field modulus`);
  });

  /**
   * The WASM SDK rejects with a bare string, so this is the shape the real
   * decode failure arrives in — narrowing to `Error` would silently drop it.
   */
  it('appends a cause that is not an Error, which is what the SDK throws', () => {
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0x' + 'f'.repeat(64),
      reason: 'it is not a readable field element',
      cause: 'Error instantiating Word from hex: value >= field modulus',
    });

    expect(describeWithCause(error)).toBe(`${error.message}: Error instantiating Word from hex: value >= field modulus`);
  });

  it('returns the message alone when there is no cause', () => {
    const error = new ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0xnope',
      reason: 'expected a 32-byte hex word',
    });

    expect(describeWithCause(error)).toBe(error.message);
  });

  /**
   * The unresolvable error's message ends in remediation that is false wherever
   * this helper is used: `switch_guardian` recovers and executes the proposal.
   */
  it('quotes the unresolvable diagnosis without its remediation', () => {
    const error = new ProposalAuthArgUnresolvableError({
      proposalId: '0xaaaa',
      signedAuthArgHex: '0xf00d',
      saltHex: '0xbeef',
      feeFaucetIdHex: '0xcafe',
    });

    expect(describeWithCause(error)).toBe(error.diagnosis);
    expect(describeWithCause(error)).not.toContain('Recreate the proposal');
    expect(error.message).toContain('Recreate the proposal');
  });
});
