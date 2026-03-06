import {
  Felt,
  FeltArray,
  Rpo256,
  type Word,
} from '@miden-sdk/miden-sdk';

export class RpoRandomCoin {
  private readonly seed: Word;

  constructor(seed: Word) {
    this.seed = seed;
  }

  drawWord(): Word {
    return Rpo256.hashElements(new FeltArray([
      ...this.seed.toFelts(),
      new Felt(0n),
      new Felt(0n),
      new Felt(0n),
      new Felt(0n),
    ]));
  }
}
