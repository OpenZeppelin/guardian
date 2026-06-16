import { Word } from '@miden-sdk/miden-sdk';

export function wordToHex(word: Word): string {
  return word.toHex();
}

export function wordElementToBigInt(
  word: { toFelts: () => Array<{ asInt: () => bigint }> },
  index: number,
): bigint {
  const felts = word.toFelts();
  return index >= 0 && index < felts.length ? felts[index].asInt() : 0n;
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

