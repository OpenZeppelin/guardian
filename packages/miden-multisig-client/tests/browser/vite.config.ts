import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { defineConfig } from 'vite';

// Serve the built package and WASM SDK from the package root.
const pkgRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

export default defineConfig({
  root: pkgRoot,
  worker: { format: 'es' },
  assetsInclude: ['**/*.wasm'],
  optimizeDeps: { exclude: ['@miden-sdk/miden-sdk'] },
  server: { port: 5599, strictPort: true },
});
