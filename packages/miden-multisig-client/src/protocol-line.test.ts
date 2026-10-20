import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PROTOCOL_LINE } from '@openzeppelin/guardian-client';
import { describe, expect, it } from 'vitest';

/**
 * The protocol line a sealed request declares must be the line of the web SDK that built the
 * request, as the Rust SDK pins it to the locked `miden-protocol` crate.
 */
describe('the protocol line', () => {
  it('matches the pinned web SDK version', () => {
    const manifest = JSON.parse(
      readFileSync(join(dirname(fileURLToPath(import.meta.url)), '../package.json'), 'utf8'),
    );
    const version: string = manifest.dependencies['@miden-sdk/miden-sdk'];
    expect(version, `protocol line ${PROTOCOL_LINE} does not match @miden-sdk/miden-sdk ${version}`).toMatch(
      new RegExp(`^${PROTOCOL_LINE.replace('.', '\\.')}\\.`),
    );
  });
});
