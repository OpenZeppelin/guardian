/// <reference types="vite/client" />

import type { SmokeApi } from './smokeHarness';

declare global {
  interface Window {
    smoke?: SmokeApi;
  }
}

export {};
