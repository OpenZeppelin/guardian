import { describe, expect, it } from 'vitest';

import { loadManifest, selectScenarios, appliesTo } from './manifest.js';
import { buildResult, isoDuration } from './report.js';
import { HANDLERS } from './runner.js';

const manifest = loadManifest();

describe('exported manifest', () => {
  it('is consumable and non-empty', () => {
    expect(manifest.scenarios.length).toBeGreaterThan(0);
    expect(manifest.networks.length).toBeGreaterThan(0);
  });

  it('carries the enum spellings this driver branches on', () => {
    for (const scenario of manifest.scenarios) {
      expect(['deterministic', 'live']).toContain(scenario.profile);
      expect(['rust', 'typescript', 'both']).toContain(scenario.sdk);
      expect(['falcon', 'ecdsa', 'mixed', 'n/a']).toContain(scenario.scheme);
      expect(['1-of-1', '2-of-3', '3-of-3', 'n/a']).toContain(scenario.shape);
      expect(['online', 'offline', 'n/a']).toContain(scenario.mode);
    }
  });

  it('declares a runtime for every scenario this driver can be asked to run', () => {
    for (const scenario of manifest.scenarios) {
      if (appliesTo(scenario, 'typescript')) {
        expect(scenario.runtime, `${scenario.id} has no runtime`).not.toBeNull();
      }
    }
  });

  it('never asks this driver for a mixed-scheme account', () => {
    const mixed = manifest.scenarios.filter((scenario) => scenario.scheme === 'mixed');
    expect(mixed).toEqual([]);
  });
});

describe('fixture commitment comparison', () => {
  const fixture = '0x552759e44efe4db81e0e699f5aea5b04b099e3bce84f4e369e4de0bb5ebb9cd9';

  it('does not let spelling decide the answer', async () => {
    const { sameCommitment } = await import('./actions/account.js');
    expect(sameCommitment(fixture, fixture.toUpperCase())).toBe(true);
    expect(sameCommitment(fixture, fixture.replace(/^0x/, ''))).toBe(true);
    expect(sameCommitment(`  ${fixture}  `, fixture)).toBe(true);
  });

  // The case the presence check could not see: a well-formed commitment that is
  // not the one the account was registered with.
  it('refuses a different well-formed commitment', async () => {
    const { sameCommitment } = await import('./actions/account.js');
    const other = '0xd49fcc29db562df747ff38ec96aeb4e20f2965d4cad72952dfef7b922ca7cff0';
    expect(sameCommitment(other, fixture)).toBe(false);
  });

  it('treats nothing as no match', async () => {
    const { sameCommitment } = await import('./actions/account.js');
    expect(sameCommitment('', '')).toBe(false);
    expect(sameCommitment('0x', fixture)).toBe(false);
  });
});

describe('handler registry', () => {
  it('names only actions the manifest actually uses', () => {
    const declared = new Set(manifest.scenarios.flatMap((scenario) => scenario.actions));
    for (const action of Object.keys(HANDLERS)) {
      expect(declared, `handler ${action} matches no declared action`).toContain(action);
    }
  });
});

describe('scenario selection', () => {
  it('returns only scenarios for the requested profile and sdk', () => {
    const selected = selectScenarios(manifest, { profile: 'deterministic', sdk: 'typescript' });
    expect(selected.length).toBeGreaterThan(0);
    for (const scenario of selected) {
      expect(scenario.profile).toBe('deterministic');
      expect(appliesTo(scenario, 'typescript')).toBe(true);
    }
  });

  it('honours an explicit id filter', () => {
    const [first] = selectScenarios(manifest, { profile: 'deterministic', sdk: 'typescript' });
    const selected = selectScenarios(manifest, {
      profile: 'deterministic',
      sdk: 'typescript',
      ids: [first.id],
    });
    expect(selected.map((scenario) => scenario.id)).toEqual([first.id]);
  });
});

describe('result construction', () => {
  it('renders durations the way the Rust driver does', () => {
    expect(isoDuration(0)).toBe('PT0S');
    expect(isoDuration(90_000)).toBe('PT1M30S');
    expect(isoDuration(3_600_000)).toBe('PT1H');
  });

  it('always records that submissions may have been retried below this level', () => {
    const result = buildResult({
      scenarioId: 'x',
      runtime: 'server-side',
      outcome: 'passed',
      durationMs: 1000,
    });
    expect(result.embedded_retry).toBe(true);
  });

  it('refuses a non-passing result without a reason', () => {
    expect(() =>
      buildResult({ scenarioId: 'x', runtime: 'server-side', outcome: 'skipped', durationMs: 0 }),
    ).toThrow(/without a reason/);
  });

  it('refuses a failure without a classification', () => {
    expect(() =>
      buildResult({
        scenarioId: 'x',
        runtime: 'server-side',
        outcome: 'failed',
        reason: 'boom',
        durationMs: 0,
      }),
    ).toThrow(/without a classification/);
  });
});

// Uses a live-profile action, which this deterministic driver will never
// implement, so the fallback branch stays exercised as more actions land.
describe('fail-closed behaviour', () => {
  it('fails a required scenario whose action has no implementation', async () => {
    const { runScenario } = await import('./runner.js');
    const scenario = {
      id: 'x',
      title: 't',
      profile: 'deterministic',
      sdk: 'typescript',
      runtime: 'server-side',
      scheme: 'n/a',
      shape: 'n/a',
      mode: 'n/a',
      actions: ['asset-transfer'],
      step_budget: 'PT1S',
      required: true,
      core: false,
    } as const;
    const result = await runScenario(scenario, {
      httpEndpoint: 'http://127.0.0.1:1',
      grpcEndpoint: '',
      imageRevision: '',
    });
    expect(result.outcome).toBe('failed');
    expect(result.classification).toBe('setup');
  });

  it('only skips an optional scenario whose action has no implementation', async () => {
    const { runScenario } = await import('./runner.js');
    const scenario = {
      id: 'x',
      title: 't',
      profile: 'deterministic',
      sdk: 'typescript',
      runtime: 'server-side',
      scheme: 'n/a',
      shape: 'n/a',
      mode: 'n/a',
      actions: ['asset-transfer'],
      step_budget: 'PT1S',
      required: false,
      core: false,
    } as const;
    const result = await runScenario(scenario, {
      httpEndpoint: 'http://127.0.0.1:1',
      grpcEndpoint: '',
      imageRevision: '',
    });
    expect(result.outcome).toBe('skipped');
  });
});

describe('funding bridge', () => {
  it('delegates to the Rust driver rather than reimplementing funding', async () => {
    const source = await import('node:fs').then((fs) =>
      fs.readFileSync(new URL('./funding.ts', import.meta.url), 'utf8'),
    );
    expect(source).toContain('qualification-driver');
    expect(source).toContain('fund');
    // The treasury key stays in the subprocess environment and is never read
    // here, which is what keeps it out of this process entirely.
    expect(source).not.toContain('QUAL_TREASURY_KEY');
  });
});
