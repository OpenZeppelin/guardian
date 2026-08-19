import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    dedupe: ['@miden-sdk/miden-sdk'],
    alias: [
      {
        find: /^@getpara\/(evm|solana|cosmos)-wallet-connectors$/,
        replacement: path.resolve(__dirname, 'getpara-wallet-connectors-stub.mjs'),
      },
      { find: '@', replacement: path.resolve(__dirname, './src') },
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
    exclude: [
      '@miden-sdk/miden-sdk',
      '@openzeppelin/guardian-client',
      '@openzeppelin/miden-multisig-client',
    ],
  },
});
