import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';
import path from 'path';

const paraOptionalModules = [
  '@getpara/evm-wallet-connectors',
  '@getpara/cosmos-wallet-connectors',
  '@getpara/solana-wallet-connectors',
];

const paraOptionalStubs: Plugin = {
  name: 'para-optional-stubs',
  resolveId(id) {
    if (paraOptionalModules.includes(id)) return id;
  },
  load(id) {
    if (paraOptionalModules.includes(id)) return 'export default undefined;';
  },
};

export default defineConfig({
  plugins: [paraOptionalStubs, react(), tailwindcss()],
  define: {
    'process.env': {},
    global: 'globalThis',
  },
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
      '@miden-sdk/miden-sdk': path.resolve(__dirname, 'node_modules/@miden-sdk/miden-sdk/dist/index.js'),
      '@openzeppelin/psm-client': path.resolve(__dirname, '../../packages/psm-client/dist/index.js'),
      '@openzeppelin/miden-multisig-client': path.resolve(__dirname, '../../packages/miden-multisig-client/dist/index.js'),
      buffer: 'buffer/',
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
    esbuildOptions: {
      plugins: [
        {
          name: 'para-optional-externals',
          setup(build) {
            const filter = new RegExp(
              `^(${paraOptionalModules.map((m) => m.replace(/[/.]/g, '\\$&')).join('|')})$`,
            );
            build.onResolve({ filter }, (args) => ({
              path: args.path,
              namespace: 'para-stub',
            }));
            build.onLoad({ filter: /.*/, namespace: 'para-stub' }, () => ({
              contents: 'export default undefined;',
              loader: 'js',
            }));
          },
        },
      ],
    },
  },
});
