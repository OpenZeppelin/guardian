import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { describe, it, expect } from 'vitest';
import {
  GUARDIAN_ERROR_CODES,
  isGuardianErrorCode,
  normalizeGuardianErrorCode,
} from './error-codes.js';

describe('normalizeGuardianErrorCode', () => {
  it('maps the SCREAMING_SNAKE wire forms to their snake_case union members', () => {
    expect(normalizeGuardianErrorCode('GUARDIAN_ACCOUNT_PAUSED')).toBe('account_paused');
    expect(normalizeGuardianErrorCode('GUARDIAN_ACCOUNT_RELEASED')).toBe('account_released');
    expect(normalizeGuardianErrorCode('GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION')).toBe(
      'insufficient_operator_permission'
    );
  });

  it('passes recognized snake_case codes through', () => {
    expect(normalizeGuardianErrorCode('commitment_mismatch')).toBe('commitment_mismatch');
    expect(normalizeGuardianErrorCode('rate_limit_exceeded')).toBe('rate_limit_exceeded');
  });

  it('returns null for unknown codes instead of widening to string', () => {
    expect(normalizeGuardianErrorCode('some_future_code')).toBeNull();
    expect(normalizeGuardianErrorCode('')).toBeNull();
  });
});

describe('isGuardianErrorCode', () => {
  it('narrows members and rejects non-members', () => {
    expect(isGuardianErrorCode('account_paused')).toBe(true);
    expect(isGuardianErrorCode('GUARDIAN_ACCOUNT_PAUSED')).toBe(false);
    expect(isGuardianErrorCode('acount_paused')).toBe(false);
  });
});

describe('drift guard against GuardianError::code()', () => {
  it('the TS union matches the server vocabulary exactly', () => {
    // Source of truth: the match arms of `pub fn code(&self)` in the server's
    // error module. The OpenAPI spec is not usable here — it declares `code`
    // as a bare string with no enum (see issue #318).
    const here = dirname(fileURLToPath(import.meta.url));
    const errorRs = readFileSync(
      join(here, '../../../crates/server/src/error.rs'),
      'utf8'
    );

    const codeFnStart = errorRs.indexOf('pub fn code(&self)');
    expect(codeFnStart).toBeGreaterThan(-1);
    // The function body ends at the first line containing only `    }` after
    // the match block; slicing to the next `pub fn` is a robust over-bound.
    const nextFn = errorRs.indexOf('pub fn', codeFnStart + 1);
    const codeFn = errorRs.slice(codeFnStart, nextFn === -1 ? undefined : nextFn);

    const serverWireCodes = [...codeFn.matchAll(/"([A-Za-z_]+)"/g)].map((m) => m[1]);
    expect(serverWireCodes.length).toBeGreaterThan(0);

    const normalizedServerCodes = new Set(
      serverWireCodes.map((wire) => normalizeGuardianErrorCode(wire) ?? `UNKNOWN:${wire}`)
    );
    const unionCodes = new Set<string>(GUARDIAN_ERROR_CODES);

    // Every server code must normalize into the union (no UNKNOWN: entries),
    // and the union must not carry stale members the server no longer emits.
    expect([...normalizedServerCodes].sort()).toEqual([...unionCodes].sort());
  });
});
