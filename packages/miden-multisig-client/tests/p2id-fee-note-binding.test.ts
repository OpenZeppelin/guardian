import { describe, expect, it } from 'vitest';

import { Word } from '@miden-sdk/miden-sdk';

import { buildP2idNoteFromMetadata } from '../src/transaction/p2id.js';
import { resolveAuthArg } from '../src/transaction/feeAuth.js';

/**
 * Establishes the stake in the salt-versus-commitment routing that
 * `buildP2idTransactionRequest` performs: `p2id` is the one type where the salt
 * has a second consumer, deriving the note serial number and so the note id.
 *
 * This file shows only that the two words give different notes, which is why
 * getting the routing wrong is not a harmless relabelling — it silently changes
 * an output note the signed summary binds. The routing itself is asserted in
 * `src/transaction/p2id.test.ts`, where the note builder's argument is a hex
 * string and so still distinguishable under that file's mocked `Word.fromHex`.
 *
 * Splitting it this way is forced: the request builder cannot run here, because
 * `MidenArrays.NoteArray` is absent from the node WASM build, and the mocked
 * `Word.fromHex` returns one felt array for every input, so note ids cannot
 * differ there.
 */
// Well-formed ids differing only in their leading nibble, built on the faucet
// `src/transaction/feeAuth.test.ts` pins against the Rust cross-SDK vector.
const SENDER = '0xcde67f7701e9e9c12493c6206bc46e';
const RECIPIENT = '0xbde67f7701e9e9c12493c6206bc46e';
const FAUCET = '0xade67f7701e9e9c12493c6206bc46e';
const SALT = Word.fromHex('0x' + '11'.repeat(32));
const AMOUNT = 10n;

const noteIdForSeed = (seedHex: string): string =>
  buildP2idNoteFromMetadata(SENDER, RECIPIENT, FAUCET, AMOUNT, 1, seedHex).id().toString();

describe('p2id output note under a fee commitment', () => {
  it('gives a different note for the commitment than for the salt', () => {
    // Why the routing matters. Feeding the note builder the commitment instead
    // of the salt does not fail anywhere in the builder — it quietly produces a
    // different output note, and so a summary the cosigners never signed.
    const { authArg } = resolveAuthArg(SALT, FAUCET);

    expect(authArg.toHex()).not.toBe(SALT.toHex());
    expect(noteIdForSeed(authArg.toHex())).not.toBe(noteIdForSeed(SALT.toHex()));
  });
});
