import { defineConfig, loadEnv } from 'vite';
import react from '@vitejs/plugin-react';
import path from 'path';

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, __dirname, '');
  const guardianTarget = env.VITE_GUARDIAN_TARGET || 'http://127.0.0.1:3000';

  return {
    plugins: [react()],
    resolve: {
      alias: {
        '@': path.resolve(__dirname, './src'),
        '@miden-sdk/miden-sdk': path.resolve(
          __dirname,
          'node_modules/@miden-sdk/miden-sdk/dist/index.js',
        ),
        '@openzeppelin/guardian-operator-client': path.resolve(
          __dirname,
          '../../packages/guardian-operator-client/dist/index.js',
        ),
      },
    },
    server: {
      port: 3003,
      proxy: {
        '/guardian': {
          target: guardianTarget,
          changeOrigin: true,
          secure: false,
          rewrite: (value) => value.replace(/^\/guardian/, ''),
          cookieDomainRewrite: '',
        },
      },
      fs: {
        allow: [
          path.resolve(__dirname, '.'),
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
      exclude: ['@miden-sdk/miden-sdk', '@openzeppelin/guardian-operator-client'],
    },
  };
});
