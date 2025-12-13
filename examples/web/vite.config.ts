import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  server: {
    port: 3001,
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

