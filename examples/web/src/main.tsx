import React from 'react';
import ReactDOM from 'react-dom/client';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { ParaProvider, Environment } from '@getpara/react-sdk-lite';
import App from './App';
import { Toaster } from '@/components/ui/sonner';
import { PARA_API_KEY, PARA_ENVIRONMENT } from '@/config';
import '@getpara/react-sdk-lite/styles.css';
import './index.css';

const queryClient = new QueryClient();

const paraEnv = PARA_ENVIRONMENT === 'production' ? Environment.PROD : Environment.DEV;

ReactDOM.createRoot(document.getElementById('root') as HTMLElement).render(
  <React.StrictMode>
    <QueryClientProvider client={queryClient}>
      <ParaProvider
        paraClientConfig={{ apiKey: PARA_API_KEY, env: paraEnv }}
        config={{ appName: 'Miden Multisig' }}
      >
        <App />
        <Toaster position="bottom-right" />
      </ParaProvider>
    </QueryClientProvider>
  </React.StrictMode>
);
