import { readFileSync } from 'node:fs';

import { describe, expect, it } from 'vitest';

import { ENVIRONMENT_SIGNALS, isEnvironmental } from './environment.js';
import { blocksConclusion } from './report.js';
import { reportAs } from './runner.js';

interface Fixtures {
  signals: string[];
  reasons: Array<{ name: string; reason: string; environmental: boolean }>;
}

function fixtures(): Fixtures {
  return JSON.parse(
    readFileSync(
      new URL(
        '../../../../fixtures/qualification/environment-classification.json',
        import.meta.url,
      ),
      'utf8',
    ),
  ) as Fixtures;
}

describe('isEnvironmental', () => {
  it('matches every shared classification vector', () => {
    for (const fixture of fixtures().reasons) {
      expect(isEnvironmental(fixture.reason), fixture.name).toBe(fixture.environmental);
    }
  });

  /**
   * The Rust driver reads the same list out of the same file. A signal added on
   * one side and not the other is the drift this catches.
   */
  it('carries the signal list the shared fixture declares', () => {
    expect([...ENVIRONMENT_SIGNALS]).toEqual(fixtures().signals);
  });
});

/** The prover deadline that failed a live scenario three nights running. */
const PROVER_DEADLINE =
  'executing the proposal failed: transaction proving failed: transport error: Timeout expired';

describe('reportAs', () => {
  it('classifies a live transport failure as environment', () => {
    expect(
      reportAs({ kind: 'failed', classification: 'product', reason: PROVER_DEADLINE }, true),
    ).toEqual({ kind: 'failed', classification: 'environment', reason: PROVER_DEADLINE });
  });

  it('keeps a deterministic transport failure a failure', () => {
    const outcome = { kind: 'failed', classification: 'product', reason: PROVER_DEADLINE } as const;
    expect(reportAs(outcome, false)).toEqual(outcome);
  });

  it('classifies a live setup failure carrying link evidence as environment', () => {
    const outcome = reportAs(
      {
        kind: 'failed',
        classification: 'setup',
        reason: 'cannot fund 0x01: the funding transfer failed: connection error',
      },
      true,
    );
    expect(outcome.kind === 'failed' && outcome.classification).toBe('environment');
  });

  it('still fails a live scenario that failed on its own terms', () => {
    const outcome = {
      kind: 'failed',
      classification: 'product',
      reason: 'executing the proposal failed: proposal not ready: need 2 signatures, have 1',
    } as const;
    expect(reportAs(outcome, true)).toEqual(outcome);
  });
});

/**
 * The reclassification is only worth anything if the exit gate honours it. It
 * keeps `outcome: 'failed'`, so a gate reading the outcome alone would still
 * fail the nightly for a prover timeout.
 */
describe('blocksConclusion', () => {
  it('does not block on an environment failure', () => {
    // The classification `reportAs` assigns, in the shape the report carries
    // it. The two vocabularies differ (`kind` on the way through, `outcome` in
    // the report), so this asserts the pair that actually meets the gate.
    const reclassified = reportAs(
      { kind: 'failed', classification: 'product', reason: PROVER_DEADLINE },
      true,
    );
    expect(reclassified.kind === 'failed' && reclassified.classification).toBe('environment');
    expect(blocksConclusion({ outcome: 'failed', classification: 'environment' })).toBe(false);
  });

  it('blocks on product and setup failures', () => {
    expect(blocksConclusion({ outcome: 'failed', classification: 'product' })).toBe(true);
    expect(blocksConclusion({ outcome: 'failed', classification: 'setup' })).toBe(true);
  });

  it('does not block on anything that is not a failure', () => {
    expect(blocksConclusion({ outcome: 'passed' })).toBe(false);
    expect(blocksConclusion({ outcome: 'skipped' })).toBe(false);
    expect(blocksConclusion({ outcome: 'environment_blocked' })).toBe(false);
  });
});
