import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { defineConfig } from 'vite';

// Serves the package root so the browser harness can import the built `dist/` artifact and
// the WASM `@miden-sdk/miden-sdk`. Mirrors the SDK-specific Vite settings from examples/web
// (ES workers, .wasm assets, exclude the SDK from dep pre-bundling).
const pkgRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

export default defineConfig({
  root: pkgRoot,
  // No SDK alias: let Vite resolve the package `import` export condition, which points at the
  // single-thread eager WASM build (`dist/st/eager.js`) that auto-initializes in the browser.
  worker: { format: 'es' },
  assetsInclude: ['**/*.wasm'],
  optimizeDeps: { exclude: ['@miden-sdk/miden-sdk'] },
  server: { port: 5599, strictPort: true },
});
