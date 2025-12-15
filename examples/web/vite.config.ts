import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 3001,
  },
  resolve: {
    alias: {
      // Force miden-sdk to resolve to our local node_modules, not nested copies
      '@demox-labs/miden-sdk': path.resolve(__dirname, 'node_modules/@demox-labs/miden-sdk'),
    },
  },
  build: {
    target: 'esnext',
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
      },
    },
  },
  worker: {
    target: 'esnext',
    format: 'es',
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
      },
    },
  },
  optimizeDeps: {
    exclude: ['@demox-labs/miden-sdk'],
  },
});

