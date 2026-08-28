import { describe, expect, it } from 'vitest';

import * as api from './index.js';
import type { AuthArgErrorCode, SignatureOptions } from './index.js';

/**
 * Every other test imports from a concrete source path, so the barrels are never
 * loaded and a dropped re-export is invisible. That matters most for the
 * fee-conversion surface: `SignatureOptions.feeFaucetId` is the documented way to
 * opt in, and a consumer who cannot name the type cannot use it.
 *
 * This lives under `src/` rather than `tests/` because `tsconfig.json` includes
 * only `src/**`, and the type half of the surface is checked by `tsc`, not by a
 * runtime assertion.
 */
describe('package entry point', () => {
  it('exports summaryAuthArg, which replaced summarySalt', () => {
    expect(typeof (api as unknown as Record<string, unknown>).summaryAuthArg).toBe('function');
  });

  it('exports the auth-arg errors a caller has to catch by type', () => {
    const exports = api as unknown as Record<string, unknown>;

    expect(typeof exports.ProposalAuthArgUnresolvableError).toBe('function');
    expect(typeof exports.ProposalSaltMalformedError).toBe('function');
  });

  it('exports AuthArgErrorCode, so a caller can branch on the codes exhaustively', () => {
    const codes: AuthArgErrorCode[] = ['proposal_auth_arg_unresolvable', 'proposal_salt_malformed'];

    expect(new api.ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0xnope',
      reason: 'expected a 32-byte hex word',
    }).code).toBe(codes[1]);
  });

  it('exports SignatureOptions with its fee faucet field', () => {
    const options: SignatureOptions = { feeFaucetId: '0xade67f7701e9e9c12493c6206bc46e' };

    expect(options.feeFaucetId).toBe('0xade67f7701e9e9c12493c6206bc46e');
  });
});
