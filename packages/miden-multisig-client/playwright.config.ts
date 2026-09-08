import { defineConfig } from '@playwright/test';

// Account construction needs the browser's IndexedDB-backed store.
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
