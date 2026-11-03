import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { NoteDetails, NoteFile } from '@miden-sdk/miden-sdk';

import { buildP2idNoteFromMetadata, parseP2idNoteType } from '../src/transaction/p2id.js';
import { bytesToHex } from '../src/utils/encoding.js';

interface NoteFileVector {
  sender: string;
  recipient: string;
  faucet: string;
  amount: number;
  note_type: string;
  salt: string;
  after_block: number;
  note_id: string;
  note_file_hex: string;
}

function loadVectors(): NoteFileVector[] {
  const fixturePath = fileURLToPath(
    new URL('../../../fixtures/miden-multisig-client/p2id-note-file-vectors.json', import.meta.url),
  );
  return JSON.parse(readFileSync(fixturePath, 'utf8')) as NoteFileVector[];
}

// Cross-language parity with `a_note_file_matches_the_shared_vector` in
// `crates/miden-multisig-client/src/client/io.rs`: the note `exportP2idNoteToBytes` rebuilds and
// the file it writes before the note commits.
describe('P2ID note file vectors', () => {
  for (const vector of loadVectors()) {
    it(`rebuilds the note and writes the file the Rust SDK exports (${vector.note_id})`, () => {
      const note = buildP2idNoteFromMetadata(
        vector.sender,
        vector.recipient,
        vector.faucet,
        BigInt(vector.amount),
        parseP2idNoteType(vector.note_type),
        vector.salt,
      );
      expect(note.id().toString()).toBe(vector.note_id);

      const file = NoteFile.fromExpectedNote(
        new NoteDetails(note.assets(), note.recipient()),
        note.metadata().tag(),
        vector.after_block,
      );
      expect(bytesToHex(file.serialize()).replace(/^0x/, '')).toBe(vector.note_file_hex);
    });
  }
});
