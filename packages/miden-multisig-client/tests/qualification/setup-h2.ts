import { afterAll } from 'vitest';

import { closeH2Sessions, installH2GrpcFetch } from './h2Fetch.js';

installH2GrpcFetch();

afterAll(closeH2Sessions);
