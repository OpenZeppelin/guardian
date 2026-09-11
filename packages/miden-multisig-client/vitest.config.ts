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
    // Without this, builder-mock call indices leak between tests in a file, so an
    // assertion reading `mock.calls[0]` can read a neighbouring test's call and pass
    // even when the code under test never invoked the builder. Three separate review
    // rounds found assertions that could not fail for that reason.
    clearMocks: true,
  },
});
