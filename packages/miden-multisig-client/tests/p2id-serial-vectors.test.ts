import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { Word } from '@miden-sdk/miden-sdk';

import { deriveP2idSerialNumber } from '../src/transaction/p2id.js';

interface P2idSerialVector {
  name: string;
  seed: string;
  output: string;
}

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
