import { Word } from '@demox-labs/miden-sdk';

export function wordToHex(word: Word): string {
  return word.toHex();
}

export function wordElementToBigInt(word: Word, index: number): bigint {
  const elements = word.toU64s();
  return index >= 0 && index < elements.length ? elements[index] : 0n;
}

