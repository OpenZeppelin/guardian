import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';

/**
 * Requesting and observing Guardian execution must need no Miden capability, so this package
 * stays dependency-free and never reaches for the Miden SDK. A caller brings its own signer.
 */
const here = dirname(fileURLToPath(import.meta.url));

describe('the base client has no Miden capability', () => {
  it('declares no runtime dependencies', () => {
    const manifest = JSON.parse(readFileSync(join(here, '../package.json'), 'utf8'));
    expect(manifest.dependencies ?? {}).toEqual({});
  });

  it('imports nothing from the Miden SDK', () => {
    const sources = readdirSync(here).filter((name) => name.endsWith('.ts') && !name.endsWith('.test.ts'));
    expect(sources.length).toBeGreaterThan(0);
    for (const file of sources) {
      expect(readFileSync(join(here, file), 'utf8'), file).not.toMatch(/from\s+'@miden-sdk\//);
    }
  });
});
