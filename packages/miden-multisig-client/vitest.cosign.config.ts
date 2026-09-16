import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';

import { defineConfig } from 'vitest/config';

// The cross-SDK cosigner, invoked as a subprocess by the Rust driver. It shares
// the qualification driver's aliasing and WASM setup because it has the same
// module-resolution problem every Node consumer of this package has.
const require = createRequire(import.meta.url);
const midenSdkRoot = dirname(require.resolve('@miden-sdk/miden-sdk/package.json'));
const midenWasmEntry = join(midenSdkRoot, 'dist/st/index.js');

export default defineConfig({
  resolve: {
    alias: [{ find: /^@miden-sdk\/miden-sdk$/, replacement: midenWasmEntry }],
  },
  test: {
    globals: true,
    environment: 'node',
    include: ['tests/qualification/cosign.spec.ts'],
    setupFiles: ['./tests/setup-wasm.ts', './tests/qualification/setup-h2.ts'],
    testTimeout: 300_000,
    hookTimeout: 120_000,
    fileParallelism: false,
  },
});
