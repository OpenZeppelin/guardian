import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';

import { describe, expect, it } from 'vitest';

interface P2idSerialVector {
  name: string;
  seed: string;
  output: string;
}

const require = createRequire(import.meta.url);
const sdkEntryPath = require.resolve('@miden-sdk/miden-sdk');
const sdkDistDir = dirname(sdkEntryPath);
const sdkWasmPath = join(sdkDistDir, 'assets', 'miden_client_web.wasm');

const { initSync, Word } = await import('@miden-sdk/miden-sdk/lazy');
initSync({ module: readFileSync(sdkWasmPath) });

const { deriveP2idSerialNumber } = await import('../src/transaction/p2id.js');

function loadVectors(): P2idSerialVector[] {
  const fixturePath = fileURLToPath(
    new URL('../../../fixtures/miden-multisig-client/p2id-serial-vectors.json', import.meta.url),
  );

  return JSON.parse(readFileSync(fixturePath, 'utf8')) as P2idSerialVector[];
}

describe('deriveP2idSerialNumber', () => {
  for (const vector of loadVectors()) {
    it(`matches the shared Rust vector for ${vector.name}`, () => {
      const actual = deriveP2idSerialNumber(Word.fromHex(vector.seed)).toHex();

      expect(actual).toBe(vector.output);
    });
  }
});
