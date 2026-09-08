// Must precede the SDK import: the SDK's bundled dexie captures
// `globalThis.indexedDB` at module load.
import '../src/testing/fake-indexeddb-device.js';

import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';

import { initSync } from '@miden-sdk/miden-sdk/lazy';

const require = createRequire(import.meta.url);
const sdkRootDir = dirname(require.resolve('@miden-sdk/miden-sdk/package.json'));
// 0.15.0 moved the WASM asset under the single-thread build dir (was dist/assets).
initSync({
  module: readFileSync(join(sdkRootDir, 'dist', 'st', 'assets', 'miden_client_web.wasm')),
});
