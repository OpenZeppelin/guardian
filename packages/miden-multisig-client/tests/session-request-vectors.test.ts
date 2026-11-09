import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { p256 } from '@noble/curves/nist.js';
import {
  RequestAuthPayload,
  SessionTransport,
  sessionRequestEnvelope,
  type SessionOperation,
} from '@openzeppelin/guardian-client';
import { describe, expect, it } from 'vitest';
import { AuthDigest } from '../src/utils/digest.js';
import { bytesToHex, hexToBytes } from '../src/utils/encoding.js';
import { wordToBytes } from '../src/utils/word.js';

interface Fixture {
  public_key_hex: string;
  vectors: Array<{
    name: string;
    account_id_hex: string;
    timestamp_ms: number;
    payload_hex: string;
    transport: number;
    operation: number;
    request_word_hex: string;
    envelope_hex: string;
    signature_hex: string;
  }>;
}

const fixture = JSON.parse(
  readFileSync(
    fileURLToPath(
      new URL(
        '../../../crates/shared/tests/fixtures/session_request_vectors.json',
        import.meta.url,
      ),
    ),
    'utf8',
  ),
) as Fixture;

const strip = (hex: string) => hex.replace(/^0x/, '');

describe('session request vectors (Rust parity)', () => {
  const jsonVectors = fixture.vectors.filter((vector) => vector.payload_hex !== '0x');

  it('derives the same request word and envelope from the request', () => {
    for (const vector of jsonVectors) {
      const request = JSON.parse(new TextDecoder().decode(hexToBytes(vector.payload_hex)));
      const word = AuthDigest.fromRequest(
        vector.account_id_hex,
        vector.timestamp_ms,
        RequestAuthPayload.fromRequest(request),
      );
      const wordBytes = new Uint8Array(wordToBytes(word));
      expect(strip(bytesToHex(wordBytes)), vector.name).toBe(strip(vector.request_word_hex));
      const envelope = sessionRequestEnvelope(
        vector.transport as SessionTransport,
        vector.operation as SessionOperation,
        wordBytes,
      );
      expect(strip(bytesToHex(envelope)), vector.name).toBe(strip(vector.envelope_hex));
    }
  });

  // The full cross product runs in Rust; P-256 verification in JavaScript is
  // slow enough that each signature is presented only as its own request, the
  // same request over the other transport or with another payload, and the
  // next operation over the same transport.
  it('verifies each Rust signature only over its own envelope', () => {
    const publicKey = hexToBytes(fixture.public_key_hex);
    const operationCount = new Set(fixture.vectors.map((vector) => vector.operation)).size;
    for (const signed of fixture.vectors) {
      const nextOperation = (signed.operation % operationCount) + 1;
      const presentedAs = fixture.vectors.filter(
        (presented) =>
          presented.operation === signed.operation ||
          (presented.transport === signed.transport &&
            presented.payload_hex === signed.payload_hex &&
            presented.operation === nextOperation),
      );
      for (const presented of presentedAs) {
        const accepted = p256.verify(
          hexToBytes(signed.signature_hex),
          hexToBytes(presented.envelope_hex),
          publicKey,
          { prehash: true, lowS: false },
        );
        expect(accepted, `${signed.name} as ${presented.name}`).toBe(
          signed.envelope_hex === presented.envelope_hex,
        );
      }
    }
  });
});
