/**
 * Drift guard for the backfill's static relevance screen (issue #416): the
 * screen recognizes well-known P2ID/P2IDE notes by script root and reads the
 * target (and P2IDE reclaimer) from the note storage layout. Both are
 * upstream implementation details, so this pins them against notes built by
 * the real WASM SDK — an SDK bump that moves the storage items or reworks
 * the scripts fails here instead of silently screening every real note out.
 *
 * Lives in tests/ (not src/) because it loads the real WASM bundle.
 */
import { describe, it, expect } from 'vitest';
import {
  AccountId,
  FungibleAsset,
  Note,
  NoteAssets,
  NoteAttachment,
  NoteType,
} from '@miden-sdk/miden-sdk';

import { screenNoteForAccount } from '../src/recovery/publicNoteBackfill.js';

const SENDER = '0x7b7b7b7a7b7b7b017b7b7b7b7b7b7b';
const TARGET = '0x1b1b1b1a1b1b1b011b1b1b1b1b1b1b';
const OTHER = '0x2c2c2c2a2c2c2c012c2c2c2c2c2c2c';
const FAUCET = '0x7c7c7c7c7c7c7c017c7c7c7c7c7c7c';

const id = (hex: string) => AccountId.fromHex(hex);
const assets = () => new NoteAssets([new FungibleAsset(id(FAUCET), 100n)]);

describe('backfill static relevance screen (wasm drift guard)', () => {
  it('accepts a P2ID note addressed at the account', () => {
    const note = Note.createP2IDNote(
      id(SENDER),
      id(TARGET),
      assets(),
      NoteType.Public,
      new NoteAttachment(),
    );
    expect(screenNoteForAccount(note, id(TARGET))).toBe('relevant');
  });

  it('rejects a P2ID note addressed at a different account', () => {
    const note = Note.createP2IDNote(
      id(SENDER),
      id(OTHER),
      assets(),
      NoteType.Public,
      new NoteAttachment(),
    );
    expect(screenNoteForAccount(note, id(TARGET))).toBe('irrelevant');
  });

  it('accepts a P2IDE note by target and by reclaimer, rejects unrelated', () => {
    const note = Note.createP2IDENote(
      id(SENDER),
      id(TARGET),
      assets(),
      100,
      null,
      NoteType.Public,
      new NoteAttachment(),
    );
    expect(screenNoteForAccount(note, id(TARGET))).toBe('relevant');
    // The P2IDE reclaimer (the sender here) can consume after the reclaim
    // height, so the note is relevant to it too.
    expect(screenNoteForAccount(note, id(SENDER))).toBe('relevant');
    expect(screenNoteForAccount(note, id(OTHER))).toBe('irrelevant');
  });
});
