import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
      '@miden-sdk/miden-sdk': path.resolve(__dirname, 'node_modules/@miden-sdk/miden-sdk/dist/index.js'),
      '@openzeppelin/psm-client': path.resolve(__dirname, '../../packages/psm-client/dist/index.js'),
      '@openzeppelin/miden-multisig-client': path.resolve(__dirname, '../../packages/miden-multisig-client/dist/index.js'),
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
    exclude: ['@miden-sdk/miden-sdk'],
  },
});

