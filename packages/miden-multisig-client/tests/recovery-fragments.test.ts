/**
 * Drift guard for the recovery classifier's message-fragment join key
 * (issue #414): `drainPrivateNoteBacklog` greps upstream `NoteTransportError`
 * Display text, which reaches JS only as a stringified error chain from the
 * WASM client. Pin both fragments against the string table of the shipped
 * WASM binary, so an SDK bump that rewords either message fails here instead
 * of silently misclassifying in production.
 *
 * Lives in tests/ (not src/) because it needs Node built-ins, which the
 * package's tsc build does not type for src files.
 */
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';
import { describe, it, expect } from 'vitest';
import {
  PAGINATION_GUARD_FRAGMENT,
  TRANSPORT_DISABLED_FRAGMENT,
} from '../src/recovery.js';

const require = createRequire(import.meta.url);

describe('recovery classification fragments', () => {
  it('match the error text shipped in the WASM binary', () => {
    const sdkRootDir = dirname(require.resolve('@miden-sdk/miden-sdk/package.json'));
    const wasm = readFileSync(join(sdkRootDir, 'dist', 'st', 'assets', 'miden_client_web.wasm'));

    const binaryText = wasm.toString('latin1');
    expect(binaryText).toContain(TRANSPORT_DISABLED_FRAGMENT);
    expect(binaryText).toContain(PAGINATION_GUARD_FRAGMENT);
  });
});
