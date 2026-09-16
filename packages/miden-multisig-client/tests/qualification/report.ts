import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

import type { Classification, Outcome, Runtime, ScenarioResult, Sdk } from './types.js';

export function isoDuration(milliseconds: number): string {
  const seconds = Math.max(0, Math.round(milliseconds / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = seconds % 60;
  let rendered = 'PT';
  if (hours > 0) rendered += `${hours}H`;
  if (minutes > 0) rendered += `${minutes}M`;
  if (remainder > 0 || (hours === 0 && minutes === 0)) rendered += `${remainder}S`;
  return rendered;
}

export interface ResultInput {
  readonly scenarioId: string;
  readonly runtime: Runtime;
  readonly outcome: Outcome;
  readonly reason?: string;
  readonly classification?: Classification;
  readonly durationMs: number;
}

const SDK: Sdk = 'typescript';

/**
 * `embedded_retry` is always true here: the bundled client retries submissions
 * below the level this project controls, so a TypeScript scenario cannot
 * evidence that a submission was sent exactly once.
 */
export function buildResult(input: ResultInput): ScenarioResult {
  if (input.outcome !== 'passed' && !input.reason) {
    throw new Error(`scenario ${input.scenarioId} reports ${input.outcome} without a reason`);
  }
  if (input.outcome === 'failed' && !input.classification) {
    throw new Error(`scenario ${input.scenarioId} failed without a classification`);
  }
  return {
    scenario_id: input.scenarioId,
    sdk: SDK,
    runtime: input.runtime,
    outcome: input.outcome,
    ...(input.reason ? { reason: input.reason } : {}),
    ...(input.classification ? { classification: input.classification } : {}),
    embedded_retry: true,
    duration: isoDuration(input.durationMs),
  };
}

/**
 * Emits this SDK's scenario results for the Rust driver to fold into the run.
 *
 * Deliberately not a whole run: conclusion and qualification claim are derived
 * from the required set, and that derivation lives on the Rust side. Deriving
 * it here too is how the two sides start disagreeing. `run_id` is what binds
 * these results to the run that carries them.
 */
export function writeResults(results: readonly ScenarioResult[], outDir: string, runId: string): string {
  const path = join(outDir, `${runId}-typescript.json`);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(
    path,
    `${JSON.stringify({ run_id: runId, scenario_results: results }, null, 2)}\n`,
  );
  return path;
}
