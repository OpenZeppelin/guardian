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
    ];

    expect(new api.ProposalSaltMalformedError({
      proposalId: '0xaaaa',
      saltHex: '0xnope',
      reason: 'expected a 32-byte hex word',
    }).code).toBe(codes[0]);
    expect(new api.MultisigAuthArgsMissingError('0xaaaa').code).toBe(codes[1]);
    expect(new api.BoundBlockNotDeclaredError(7).code).toBe(codes[2]);
  });

  it('exports the tip-execution helper a multisig proposal is reproduced with', () => {
    expect(typeof api.executeForSummaryAtTip).toBe('function');
  });

  it('exports the Guardian execution errors, wait defaults and terminal-state helper', () => {
    expect(typeof api.GuardianExecutionRefusedError).toBe('function');
    expect(typeof api.GuardianExecutionWaitTimeoutError).toBe('function');
    expect(api.DEFAULT_EXECUTION_WAIT_OPTIONS.deadlineMs).toBe(900_000);
    expect(api.isTerminalExecutionState('committed')).toBe(true);
  });

  it('exports the local-execution rule, its reasons and its errors', () => {
    const reasons: api.LocalExecutionReason[] = ['switch_guardian', 'private_note'];
    expect(reasons.map(api.describeLocalExecutionReason)).toHaveLength(2);
    expect(typeof api.localExecutionReason).toBe('function');
    expect(new api.LocalExecutionRequiredError({ proposalId: '0xaaaa', reason: 'private_note' }).reason).toBe(
      'private_note',
    );
    expect(new api.ProposalNotHeldLocallyError('0xaaaa').proposalId).toBe('0xaaaa');
  });
});
