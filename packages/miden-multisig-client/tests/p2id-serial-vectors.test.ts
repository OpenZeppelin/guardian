import { readFileSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

interface P2idSerialVector {
  name: string;
  seed: string;
  output: string;
}

const originalFetch = globalThis.fetch.bind(globalThis);

globalThis.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
  const url =
    input instanceof URL
      ? input
      : typeof input === 'string'
        ? new URL(input)
        : new URL(input.url);

  if (url.protocol === 'file:') {
    const wasm = await readFile(fileURLToPath(url));
    return new Response(wasm, {
      status: 200,
      headers: { 'Content-Type': 'application/wasm' },
    });
  }

  return originalFetch(input, init);
};

const { Word } = await import('@miden-sdk/miden-sdk');
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
