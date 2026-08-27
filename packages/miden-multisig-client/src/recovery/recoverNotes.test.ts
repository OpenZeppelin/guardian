import { describe, expect, it, vi } from 'vitest';

import {
  runNoteRecovery,
  type NoteRecoverySteps,
} from './recoverNotes.js';
import type { NoteImportOutcome } from './proposalNoteImport.js';
import type { PublicBackfillReport } from './publicNoteBackfill.js';
import type { TransportRecoveryReport } from './transportDrain.js';

const cleanTransport: TransportRecoveryReport = {
  status: 'completed',
  imported: 2,
  retryable: false,
};

function outcome(overrides: Partial<NoteImportOutcome> = {}): NoteImportOutcome {
  return {
    identifier: '0xabc',
    source: 'proposal',
    status: 'imported',
    ...overrides,
  };
}

function backfillReport(overrides: Partial<PublicBackfillReport> = {}): PublicBackfillReport {
  return {
    scannedFrom: 0,
    scannedTo: 100,
    discovered: 1,
    skippedPrivate: 0,
    skippedIrrelevant: 0,
    outcomes: [outcome({ source: 'backfill' })],
    uncovered: [],
    retryable: false,
    ...overrides,
  };
}

function steps(overrides: Partial<NoteRecoverySteps> = {}): NoteRecoverySteps {
  return {
    transportDrain: vi.fn(async () => cleanTransport),
    proposalImport: vi.fn(async () => [outcome(), outcome({ status: 'already-present' })]),
    publicBackfill: vi.fn(async () => backfillReport()),
    sync: vi.fn(async () => {}),
    ...overrides,
  };
}

describe('runNoteRecovery', () => {
  it('runs every strategy by default and aggregates the reports', async () => {
    const s = steps();
    const report = await runNoteRecovery({}, s);

    expect(s.transportDrain).toHaveBeenCalledOnce();
    expect(s.proposalImport).toHaveBeenCalledOnce();
    expect(s.publicBackfill).toHaveBeenCalledOnce();
    expect(s.sync).toHaveBeenCalledOnce();

    expect(report.transport).toEqual(cleanTransport);
    expect(report.proposalImport).toHaveLength(2);
    expect(report.backfill?.discovered).toBe(1);
    expect(report.problems).toEqual([]);
    expect(report.synced).toBe(true);
    // 2 from the drain + 1 imported proposal outcome + 1 imported backfill
    // outcome; the already-present outcome does not count.
    expect(report.imported).toBe(4);
    expect(report.retryable).toBe(false);
  });

  it('skips disabled strategies without touching their steps', async () => {
    const s = steps();
    const report = await runNoteRecovery(
      { proposalImport: false, publicBackfill: false, syncAfter: false },
      s,
    );

    expect(s.transportDrain).toHaveBeenCalledOnce();
    expect(s.proposalImport).not.toHaveBeenCalled();
    expect(s.publicBackfill).not.toHaveBeenCalled();
    expect(s.sync).not.toHaveBeenCalled();

    expect(report.proposalImport).toBeUndefined();
    expect(report.backfill).toBeUndefined();
    expect(report.synced).toBe(false);
    expect(report.problems).toEqual([]);
    expect(report.imported).toBe(2);
  });

  it('folds step throws into problems and keeps going', async () => {
    const s = steps({
      transportDrain: vi.fn(async () => {
        throw new Error('store exploded');
      }),
      proposalImport: vi.fn(async () => {
        throw new Error('guardian unreachable');
      }),
      sync: vi.fn(async () => {
        throw new Error('sync flaked');
      }),
    });
    const report = await runNoteRecovery({}, s);

    // The backfill still ran after the earlier steps failed.
    expect(s.publicBackfill).toHaveBeenCalledOnce();
    expect(report.transport).toBeUndefined();
    expect(report.proposalImport).toBeUndefined();
    expect(report.backfill).toBeDefined();
    expect(report.synced).toBe(false);

    expect(report.problems.map((p) => p.step)).toEqual([
      'transport-drain',
      'proposal-import',
      'sync',
    ]);
    // A drain throw means the local store failed — not retryable; the
    // GUARDIAN listing and the sync are I/O failures — retryable.
    expect(report.problems.map((p) => p.retryable)).toEqual([false, true, true]);
    expect(report.problems[1]?.reason).toContain('guardian unreachable');
    expect(report.retryable).toBe(true);
  });

  it('marks the report retryable when a strategy report is retryable', async () => {
    const s = steps({
      publicBackfill: vi.fn(async () =>
        backfillReport({
          outcomes: [],
          discovered: 0,
          uncovered: [{ from: 5, to: 9 }],
          retryable: true,
          reason: 'blocks [5, 9]: node hiccup',
        }),
      ),
    });
    const report = await runNoteRecovery({}, s);

    expect(report.problems).toEqual([]);
    expect(report.retryable).toBe(true);
  });

  it('marks the report retryable from a retryable per-note outcome', async () => {
    const s = steps({
      proposalImport: vi.fn(async () => [
        outcome({ status: 'not-committed', retryable: true }),
      ]),
    });
    const report = await runNoteRecovery({}, s);

    expect(report.imported).toBe(3);
    expect(report.retryable).toBe(true);
  });

  it('marks a backfill range error non-retryable, unlike a tip-lookup failure', async () => {
    const { BackfillRangeError } = await import('./publicNoteBackfill.js');
    const s = steps({
      publicBackfill: vi.fn(async () => {
        throw new BackfillRangeError('backfill range is inverted: fromBlock 9 > toBlock 5');
      }),
    });
    const report = await runNoteRecovery({}, s);

    expect(report.problems).toHaveLength(1);
    expect(report.problems[0]?.step).toBe('public-backfill');
    expect(report.problems[0]?.retryable).toBe(false);
  });

  it('rejects an inverted backfill range before running anything', async () => {
    const s = steps();
    await expect(runNoteRecovery({ fromBlock: 5, toBlock: 1 }, s)).rejects.toThrow(
      'inverted',
    );
    expect(s.transportDrain).not.toHaveBeenCalled();
    expect(s.sync).not.toHaveBeenCalled();
  });
});
