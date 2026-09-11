import { describe, expect, it } from 'vitest';

import * as api from './index.js';
import type { AuthArgErrorCode } from './index.js';

/**
 * Every other test imports from a concrete source path, so the barrels are never
 * loaded and a dropped re-export is invisible. The surface that matters is the
 * auth-arg error contract: the codes are stable identifiers a caller branches on,
 * and a caller who cannot name the type cannot branch on it at all.
 *
 * This lives under `src/` rather than `tests/` because `tsconfig.json` includes
 * only `src/**`, and the type half of the surface is checked by `tsc`, not by a
 * runtime assertion.
 */
describe('package entry point', () => {
  it('exports AuthArgErrorCode, so a caller can branch on the codes exhaustively', () => {
    const codes: AuthArgErrorCode[] = [
      'proposal_auth_arg_unresolvable',
      'proposal_salt_malformed',
      'fee_faucet_anchor_mismatch',
    ];

    expect(new api.ProposalAuthArgUnresolvableError({
      proposalId: '0xaaaa',
      signedAuthArgHex: '0xf00d',
      saltHex: '0xbeef',
      feeFaucetIdHex: '0xcafe',
    }).code).toBe(codes[0]);
    expect(new api.ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0xnope',
      reason: 'expected a 32-byte hex word',
    }).code).toBe(codes[1]);
  });
});
