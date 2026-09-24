import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';

import { defineConfig } from 'vitest/config';

// The qualification driver, kept out of the default suite: it needs a running
// stack, and `npm test` must stay runnable without one. Aliasing and WASM
// initialization are shared with vitest.config.ts, because the driver has the
// same module-resolution problem every consumer of this package has in Node.
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
    include: ['tests/qualification/driver.spec.ts'],
    setupFiles: ['./tests/setup-wasm.ts', './tests/qualification/setup-h2.ts'],
    // Per scenario, not per run. The longest step budget in the manifest is
    // 480s and a scenario also funds and waits for GUARDIAN to settle, so this
    // leaves room above that without letting a wedged scenario run forever.
    testTimeout: 900_000,
    hookTimeout: 120_000,
    fileParallelism: false,
  },
});
