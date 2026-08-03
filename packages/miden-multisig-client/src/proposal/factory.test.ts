import { describe, expect, it } from 'vitest';
import type { DeltaStatus } from '@openzeppelin/guardian-client';
import { ProposalFactory } from './factory.js';

describe('ProposalFactory.toStatus', () => {
  const factory = new ProposalFactory({
    accountId: '0x' + 'a'.repeat(30),
    signerCommitments: [],
    resolveRequiredSignatures: () => 2,
  });
  // TS `private` is compile-time only; the mapping is worth pinning
  // without dragging a full tx summary through `fromDelta`.
  const toStatus = (status: DeltaStatus) =>
    (factory as unknown as { toStatus: (s: DeltaStatus, t: string, sigs: unknown[]) => string })
      .toStatus(status, 'p2id', []);

  it('maps every terminal delta status to finalized (issue #345)', () => {
    // A retained delta left the active candidate path: no longer
    // signable, so finalized here — whether it landed is resolved by
    // background reconciliation via the delta status, not the proposal
    // list.
    expect(toStatus({ status: 'canonical', timestamp: 't' })).toBe('finalized');
    // Cast until the published guardian-client types include `retained`.
    expect(
      toStatus({
        status: 'retained',
        timestamp: 't',
        reason: 'retry_exhausted',
      } as unknown as DeltaStatus),
    ).toBe('finalized');
    expect(toStatus({ status: 'discarded', timestamp: 't' })).toBe('finalized');
  });

  it('maps active statuses to pending/ready', () => {
    expect(toStatus({ status: 'candidate', timestamp: 't' })).toBe('ready');
    expect(
      toStatus({ status: 'pending', timestamp: 't', proposerId: 'p', cosignerSigs: [] }),
    ).toBe('pending');
  });
});
