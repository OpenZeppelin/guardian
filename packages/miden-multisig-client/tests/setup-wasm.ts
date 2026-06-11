import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createRequire } from 'node:module';

import { initSync } from '@miden-sdk/miden-sdk/lazy';

const require = createRequire(import.meta.url);
const sdkRootDir = dirname(require.resolve('@miden-sdk/miden-sdk/package.json'));
initSync({ module: readFileSync(join(sdkRootDir, 'dist', 'assets', 'miden_client_web.wasm')) });
