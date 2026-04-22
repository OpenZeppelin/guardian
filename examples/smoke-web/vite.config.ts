import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

export default defineConfig({
  plugins: [react()],
  resolve: {
    dedupe: ['react', 'react-dom', '@tanstack/react-query'],
    alias: {
      '@': path.resolve(__dirname, './src'),
      '@multisig-browser': path.resolve(__dirname, '../_shared/multisig-browser/src'),
      '@miden-sdk/miden-sdk': path.resolve(
        __dirname,
        'node_modules/@miden-sdk/miden-sdk/dist/index.js',
      ),
      '@openzeppelin/guardian-client': path.resolve(
        __dirname,
        '../../packages/guardian-client/dist/index.js',
      ),
      '@openzeppelin/miden-multisig-client': path.resolve(
        __dirname,
        '../../packages/miden-multisig-client/dist/index.js',
      ),
    },
  },
  server: {
    port: 3002,
    fs: {
      allow: [
        path.resolve(__dirname, '.'),
        path.resolve(__dirname, '../_shared'),
        path.resolve(__dirname, '../../packages'),
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
    exclude: [
      '@miden-sdk/miden-sdk',
      '@openzeppelin/guardian-client',
      '@openzeppelin/miden-multisig-client',
    ],
  },
});
