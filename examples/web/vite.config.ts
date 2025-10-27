import { defineConfig } from 'vite';

export default defineConfig({
  server: {
    port: 5173,
  },
  build: {
    target: 'esnext',
  },
  optimizeDeps: {
    exclude: ['@demox-labs/miden-sdk'],
  },
});
