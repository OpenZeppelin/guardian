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
      'proposal_salt_malformed',
      'multisig_auth_args_missing',
      'bound_block_not_declared',
      'transaction_summary_layout_unsupported',
      'bound_block_mismatch',
    ];

    expect(new api.ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0xnope',
      reason: 'expected a 32-byte hex word',
    }).code).toBe(codes[0]);
    expect(new api.MultisigAuthArgsMissingError('0xaaaa').code).toBe(codes[1]);
    expect(new api.BoundBlockNotDeclaredError(7).code).toBe(codes[2]);
    expect(new api.TransactionSummaryLayoutError('version 2').code).toBe(codes[3]);
    expect(new api.BoundBlockMismatchError({
      proposalId: '0xaaaa',
      declaredBoundBlockNum: 8,
      boundBlockNum: 7,
    }).code).toBe(codes[4]);
  });

  it('exports the tip-execution helper a multisig proposal is reproduced with', () => {
    expect(typeof api.executeForSummaryAtTip).toBe('function');
  });

  it('exports the reader for the block a summary binds', () => {
    expect(typeof api.summaryBoundBlockNum).toBe('function');
  });
});
