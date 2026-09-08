import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

export default defineConfig({
  plugins: [react()],
  resolve: {
    dedupe: [
      'react',
      'react-dom',
      '@miden-sdk/miden-sdk',
    ],
    alias: [
      { find: '@', replacement: path.resolve(__dirname, './src') },
      {
        find: '@multisig-browser',
        replacement: path.resolve(__dirname, '../_shared/multisig-browser/src'),
      },
      {
        find: '@openzeppelin/guardian-client',
        replacement: path.resolve(__dirname, '../../packages/guardian-client/dist/index.js'),
      },
      {
        find: '@openzeppelin/miden-multisig-client',
        replacement: path.resolve(__dirname, '../../packages/miden-multisig-client/dist/index.js'),
      },
    ],
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
