import { defineConfig } from '@playwright/test';

// Browser-based cross-SDK determinism gate. Uses the system Chrome (channel: 'chrome') so no
// Chromium download is needed, and a Vite dev server to serve the WASM SDK + built package to
// the harness page. This is the gate that the node/vitest harness cannot provide (account
// construction needs a browser IndexedDB store).
export default defineConfig({
  testDir: './tests/browser',
  testMatch: '**/*.spec.ts',
  timeout: 180_000,
  fullyParallel: false,
  workers: 1,
  reporter: 'list',
  use: {
    channel: 'chrome',
    baseURL: 'http://localhost:5599',
  },
  webServer: {
    command: 'npx vite --config tests/browser/vite.config.ts',
    url: 'http://localhost:5599/tests/browser/harness.html',
    timeout: 180_000,
    reuseExistingServer: false,
  },
});
