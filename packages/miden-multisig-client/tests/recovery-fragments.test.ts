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
  NODE_RPC_FRAGMENT,
  NOTE_TRANSPORT_COVERED_TAGS_KEY,
  PAGINATION_GUARD_FRAGMENT,
  STORE_ERROR_FRAGMENT,
  TRANSPORT_CONNECTION_FRAGMENT,
  TRANSPORT_DISABLED_FRAGMENT,
  TRANSPORT_NETWORK_FRAGMENT,
} from '../src/recovery/transportDrain.js';
import { RPC_PAGINATION_FRAGMENT } from '../src/recovery/publicNoteBackfill.js';

const require = createRequire(import.meta.url);

describe('recovery classification fragments', () => {
  it('match the error text shipped in the WASM binary', () => {
    const sdkRootDir = dirname(require.resolve('@miden-sdk/miden-sdk/package.json'));
    const wasm = readFileSync(join(sdkRootDir, 'dist', 'st', 'assets', 'miden_client_web.wasm'));

    const binaryText = wasm.toString('latin1');
    expect(binaryText).toContain(TRANSPORT_DISABLED_FRAGMENT);
    expect(binaryText).toContain(PAGINATION_GUARD_FRAGMENT);
    expect(binaryText).toContain(STORE_ERROR_FRAGMENT);
    expect(binaryText).toContain(RPC_PAGINATION_FRAGMENT);
    expect(binaryText).toContain(TRANSPORT_NETWORK_FRAGMENT);
    expect(binaryText).toContain(TRANSPORT_CONNECTION_FRAGMENT);
    expect(binaryText).toContain(NODE_RPC_FRAGMENT);
    // Not an error fragment but the same class of stringly cross-binary
    // contract: `settings.remove` of a renamed key would succeed silently
    // and degrade the drain to a no-op that still reports 'completed'.
    expect(binaryText).toContain(NOTE_TRANSPORT_COVERED_TAGS_KEY);
  });
});
