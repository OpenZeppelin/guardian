import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
      '@demox-labs/miden-sdk': path.resolve(__dirname, 'node_modules/@demox-labs/miden-sdk/dist/index.js'),
    },
  },
  server: {
    port: 3001,
    fs: {
      // allow serving files from workspace and parent packages
      allow: [
        path.resolve(__dirname, '.'), // workspace (includes vendor/)
        path.resolve(__dirname, '../../packages'), // sibling packages
      ],
    },
  },
  build: {
    target: 'esnext',
    rollupOptions: {
      output: {
        inlineDynamicImports: true,
        assetFileNames: '[name][extname]',
      },
    },
  },
  worker: {
    format: 'es',
  },
  assetsInclude: ['**/*.wasm'],
  optimizeDeps: {
    exclude: ['@demox-labs/miden-sdk'],
  },
});

