import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';

import { defineConfig } from 'vitest/config';

// Tests must run against the WASM build (used in production), not the native
// napi build that the "node" condition resolves, which omits `Poseidon2`/
// `FeltArray`. Alias the bare specifier to the WASM single-thread entry and
// initialize its module in `setupFiles`.
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
    include: ['src/**/*.test.ts', 'tests/**/*.test.ts'],
    setupFiles: ['./tests/setup-wasm.ts'],
  },
});
