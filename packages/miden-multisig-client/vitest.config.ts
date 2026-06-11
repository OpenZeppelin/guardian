import { fileURLToPath } from 'node:url';

import { defineConfig } from 'vitest/config';

// The 0.15 miden-sdk ships two builds: a native napi build (resolved by the
// "node" condition) that omits WASM-only helpers like `Poseidon2`/`FeltArray`,
// and the WASM build used in browsers. The SDK is consumed as the WASM build in
// production, so tests must exercise that same build. Alias the bare specifier
// to the WASM entry (matching `@miden-sdk/miden-sdk/lazy`) and initialize its
// WASM module once in `setupFiles`.
const midenWasmEntry = fileURLToPath(
  new URL('./node_modules/@miden-sdk/miden-sdk/dist/index.js', import.meta.url),
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
