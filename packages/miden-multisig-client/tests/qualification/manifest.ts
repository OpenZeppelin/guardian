import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { ExportedManifest, Scenario, Sdk } from './types.js';

const here = dirname(fileURLToPath(import.meta.url));

/**
 * The manifest is parsed and validated once, by the Rust driver, and consumed
 * here in its generated JSON form. A second parser would be a second place for
 * the two drivers to disagree about what a scenario means.
 */
export function manifestPath(): string {
  return join(here, '../../../../qualification/manifest/manifest.json');
}

export function loadManifest(path = manifestPath()): ExportedManifest {
  const raw = readFileSync(path, 'utf8');
  const parsed = JSON.parse(raw) as ExportedManifest;
  if (!Array.isArray(parsed.scenarios) || !Array.isArray(parsed.networks)) {
    throw new Error(`${path} is not an exported qualification manifest`);
  }
  return parsed;
}

export function appliesTo(scenario: Scenario, sdk: Sdk): boolean {
  return scenario.sdk === 'both' || scenario.sdk === sdk;
}

export function selectScenarios(
  manifest: ExportedManifest,
  options: { profile: Scenario['profile']; sdk: Sdk; ids?: readonly string[] },
): readonly Scenario[] {
  return manifest.scenarios
    .filter((scenario) => scenario.profile === options.profile)
    .filter((scenario) => appliesTo(scenario, options.sdk))
    .filter((scenario) => !options.ids?.length || options.ids.includes(scenario.id));
}
