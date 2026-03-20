import React from 'react';
import ReactDOM from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { Environment, ParaProvider } from '@getpara/react-sdk-lite';
import App from './App';
import { PARA_API_KEY, PARA_ENVIRONMENT } from './config';
import '@getpara/react-sdk-lite/styles.css';
import './index.css';

const queryClient = new QueryClient();
const paraEnvironment = PARA_ENVIRONMENT === 'production' ? Environment.PROD : Environment.DEV;

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ParaProvider
        paraClientConfig={{ apiKey: PARA_API_KEY, env: paraEnvironment }}
        config={{ appName: 'Miden Multisig Smoke' }}
      >
        <App />
      </ParaProvider>
    </QueryClientProvider>
  </React.StrictMode>,
);
