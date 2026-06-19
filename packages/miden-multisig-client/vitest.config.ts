import { fileURLToPath } from 'node:url';

import { defineConfig } from 'vitest/config';

// Tests must run against the WASM build (used in production), not the native
// napi build that the "node" condition resolves, which omits `Poseidon2`/
// `FeltArray`. Alias the bare specifier to the WASM single-thread entry and
// initialize its module in `setupFiles`.
const midenWasmEntry = fileURLToPath(
  new URL('./node_modules/@miden-sdk/miden-sdk/dist/st/index.js', import.meta.url),
);

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
