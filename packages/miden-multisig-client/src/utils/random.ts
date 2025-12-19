import { Word } from '@demox-labs/miden-sdk';

export function randomWord(): Word {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  const view = new DataView(bytes.buffer);
  const u64s = new BigUint64Array([
    view.getBigUint64(0, true),
    view.getBigUint64(8, true),
    view.getBigUint64(16, true),
    view.getBigUint64(24, true),
  ]);
  return new Word(u64s);
}

