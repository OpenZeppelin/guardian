import { describe, it, expect, vi, beforeEach } from 'vitest';

// The Miden SDK is wasm-bound and is mocked across this test suite (matching
// the pattern in transaction.test.ts, multisig.test.ts, etc.). We cannot run
// real RPO256 here, but we CAN verify the lookup-digest function builds the
// FeltArray sequence in the right order and chunking convention. Cross-language
// parity against `crates/shared/tests/fixtures/lookup_auth_vectors.json` is
// intentionally a separate runner — see `runRealSdkParity` below for the gated
// hook.

interface FakeFelt {
  __felt: true;
  value: bigint;
}

interface FakeWord {
  __word: true;
  felts: FakeFelt[];
  toFelts: () => FakeFelt[];
  toHex: () => string;
}

const hashCalls: { input: FakeFelt[] }[] = [];

vi.mock('@miden-sdk/miden-sdk', () => {
  const Felt = vi.fn().mockImplementation((value: bigint) => ({
    __felt: true,
    value,
  }));

  const FeltArray = vi.fn().mockImplementation((felts: FakeFelt[]) => felts);

  const makeWord = (felts: FakeFelt[], hex = '0xMOCKED'): FakeWord => ({
    __word: true,
    felts,
    toFelts: () => felts,
    toHex: () => hex,
  });

  const Word = {
    fromHex: vi.fn().mockImplementation((hex: string) => {
      // Deterministic four-felt decomposition: split hex into 4 8-byte pieces.
      // The values are not real Word semantics, but they let us assert the
      // implementation passes them through in order.
      const stripped = hex.startsWith('0x') ? hex.slice(2) : hex;
      const padded = stripped.padStart(64, '0');
      const felts: FakeFelt[] = [];
      for (let i = 0; i < 4; i += 1) {
        const piece = padded.slice(i * 16, (i + 1) * 16);
        felts.push({ __felt: true, value: BigInt('0x' + piece) });
      }
      return makeWord(felts, hex);
    }),
  };

  const Rpo256 = {
    hashElements: vi.fn().mockImplementation((felts: FakeFelt[]) => {
      hashCalls.push({ input: [...felts] });
      // Synthesize a fake digest: 4 felts derived from the input length and
      // first felt's value, so each call's output is distinguishable.
      const seed = felts.length === 0 ? 0n : felts[0].value;
      return makeWord(
        [
          { __felt: true, value: seed ^ BigInt(felts.length) },
          { __felt: true, value: seed + 1n },
          { __felt: true, value: seed + 2n },
          { __felt: true, value: seed + 3n },
        ],
        '0xfakehash',
      );
    }),
  };

  return { Felt, FeltArray, Rpo256, Word };
});

const DOMAIN_TAG_BYTES = new TextEncoder().encode('guardian.lookup.v1');

function bytesToExpectedFelts(bytes: Uint8Array): bigint[] {
  const out: bigint[] = [];
  for (let offset = 0; offset < bytes.length; offset += 8) {
    let value = 0n;
    for (let i = 0; i < 8; i += 1) {
      const byte = offset + i < bytes.length ? bytes[offset + i] : 0;
      value |= BigInt(byte) << BigInt(8 * i);
    }
    out.push(value);
  }
  return out;
}

let lookupAuthDigest: typeof import('./lookupAuth.js').lookupAuthDigest;
let _resetLookupDomainTagCacheForTesting: typeof import('./lookupAuth.js')._resetLookupDomainTagCacheForTesting;

beforeEach(async () => {
  hashCalls.length = 0;
  // Re-import to pick up a fresh cached domain tag per test, in addition to
  // the explicit reset, so test ordering does not change behavior.
  const mod = await import('./lookupAuth.js');
  lookupAuthDigest = mod.lookupAuthDigest;
  _resetLookupDomainTagCacheForTesting = mod._resetLookupDomainTagCacheForTesting;
  _resetLookupDomainTagCacheForTesting();
});

describe('lookupAuthDigest', () => {
  it('hashes the domain-tag bytes via the same 8-byte LE chunking as Rust', () => {
    const keyCommitmentHex = '0x' + 'aa'.repeat(32);
    lookupAuthDigest(1_700_000_000_000, keyCommitmentHex);

    // First Rpo256.hashElements call must be the domain-tag computation
    // (3 felts for "guardian.lookup.v1" — 18 bytes → ceil(18/8) = 3 chunks).
    expect(hashCalls.length).toBeGreaterThanOrEqual(2);
    const tagCall = hashCalls[0];
    const expected = bytesToExpectedFelts(DOMAIN_TAG_BYTES);
    expect(tagCall.input).toHaveLength(expected.length);
    expect(tagCall.input.length).toBe(3);
    for (let i = 0; i < expected.length; i += 1) {
      expect(tagCall.input[i].value).toBe(expected[i]);
    }
  });

  it('caches the domain-tag computation across calls', () => {
    const keyCommitmentHex = '0x' + 'bb'.repeat(32);
    lookupAuthDigest(1_700_000_000_000, keyCommitmentHex);
    const callsAfterFirst = hashCalls.length;
    lookupAuthDigest(1_700_000_000_001, keyCommitmentHex);
    const callsAfterSecond = hashCalls.length;

    // Second invocation should perform exactly one Rpo256.hashElements call
    // (the message hash); the domain-tag hash is cached.
    expect(callsAfterSecond - callsAfterFirst).toBe(1);
  });

  it('passes 9 felts in [tag, timestamp, key_commitment] order to the message hash', () => {
    const keyCommitmentHex = '0x' + '01'.repeat(32);
    const timestamp = 1_700_000_000_000n;
    lookupAuthDigest(timestamp, keyCommitmentHex);

    const messageCall = hashCalls[hashCalls.length - 1];
    expect(messageCall.input).toHaveLength(9);

    // Felt 4 is the timestamp, reinterpreted as u64 (no change for positive).
    expect(messageCall.input[4].value).toBe(timestamp);

    // Felts 5..8 are the key-commitment felts, derived deterministically by
    // the mocked Word.fromHex from the hex representation.
    const expectedKcWord = '01'.repeat(32);
    for (let i = 0; i < 4; i += 1) {
      const piece = expectedKcWord.slice(i * 16, (i + 1) * 16);
      expect(messageCall.input[5 + i].value).toBe(BigInt('0x' + piece));
    }
  });

  it('reinterprets a negative timestamp via two-complement u64 cast', () => {
    const keyCommitmentHex = '0x' + 'cc'.repeat(32);
    lookupAuthDigest(-1, keyCommitmentHex);

    const messageCall = hashCalls[hashCalls.length - 1];
    // -1 cast to u64 is 0xFFFFFFFFFFFFFFFF.
    expect(messageCall.input[4].value).toBe(0xFFFFFFFFFFFFFFFFn);
  });

  it('produces distinct digest inputs for distinct commitments', () => {
    lookupAuthDigest(1_700_000_000_000, '0x' + '01'.repeat(32));
    lookupAuthDigest(1_700_000_000_000, '0x' + '02'.repeat(32));

    // Call 0 is the cached domain-tag hash. Calls 1 and 2 are the two message
    // hashes; the second call hits the domain-tag cache so it does not produce
    // an extra hash call.
    const messageCallA = hashCalls[1];
    const messageCallB = hashCalls[2];
    expect(messageCallA.input[5].value).not.toBe(messageCallB.input[5].value);
  });

  it('produces distinct digest inputs for distinct timestamps', () => {
    const keyCommitmentHex = '0x' + '01'.repeat(32);
    lookupAuthDigest(1_700_000_000_000, keyCommitmentHex);
    lookupAuthDigest(1_700_000_000_001, keyCommitmentHex);

    const messageCallA = hashCalls[1];
    const messageCallB = hashCalls[2];
    expect(messageCallA.input[4].value).not.toBe(messageCallB.input[4].value);
  });
});
