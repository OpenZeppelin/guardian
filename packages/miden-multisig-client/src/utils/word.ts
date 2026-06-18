import { Word } from '@miden-sdk/miden-sdk';

export function wordToHex(word: Word): string {
  return word.toHex();
}

export function wordElementToBigInt(word: Word, index: number): bigint {
  if (index < 0 || index > 3) {
    return 0n;
  }
  // The wallet-embedded 0.15 SDK exposes `toFelts()` but not `toU64s()` on
  // storage-read Words (a published-0.15 .d.ts/glue gap), so fall back to
  // toFelts — same element order, so indices are unchanged.
  const elements: BigUint64Array | bigint[] =
    typeof word.toU64s === 'function' ? word.toU64s() : word.toFelts().map(f => f.asInt());
  return index < elements.length ? elements[index] : 0n;
}

export function wordToBytes(word: { toFelts: () => Array<{ asInt: () => bigint }> }): Uint8Array {
  const felts = word.toFelts();
  const buf = new Uint8Array(32);
  for (let i = 0; i < 4; i++) {
    const val = felts[i].asInt();
    for (let b = 0; b < 8; b++) {
      buf[i * 8 + b] = Number((val >> BigInt(b * 8)) & BigInt(0xff));
    }
  }
  return buf;
}

