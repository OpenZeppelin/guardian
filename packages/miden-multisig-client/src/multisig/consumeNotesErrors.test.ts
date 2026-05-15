import { describe, expect, it } from 'vitest';
import {
  ConsumeNotesMetadataOversizeError,
  LegacyConsumeNotesNoteMissingError,
  NoteBindingMismatchError,
  UnsupportedMetadataVersionError,
} from './consumeNotesErrors.js';

describe('consume_notes error taxonomy (issue #229)', () => {
  it('pins stable codes identical to the Rust SDK', () => {
    expect(new NoteBindingMismatchError('x').code).toBe('consume_notes_note_binding_mismatch');
    expect(new UnsupportedMetadataVersionError(99).code).toBe(
      'consume_notes_unsupported_metadata_version',
    );
    expect(new ConsumeNotesMetadataOversizeError(1, 2).code).toBe(
      'consume_notes_metadata_oversize',
    );
    expect(new LegacyConsumeNotesNoteMissingError('0xabc').code).toBe(
      'consume_notes_legacy_note_missing',
    );
  });

  it('preserves structured error fields for programmatic branching', () => {
    const oversize = new ConsumeNotesMetadataOversizeError(262_144, 300_000);
    expect(oversize.limit).toBe(262_144);
    expect(oversize.actual).toBe(300_000);

    const missing = new LegacyConsumeNotesNoteMissingError('0xnoteid');
    expect(missing.noteId).toBe('0xnoteid');

    const unsupported = new UnsupportedMetadataVersionError(99);
    expect(unsupported.found).toBe(99);
    const absent = new UnsupportedMetadataVersionError(undefined);
    expect(absent.found).toBeUndefined();
  });

  it('extends Error so instanceof Error and instanceof <subclass> both work', () => {
    const err = new NoteBindingMismatchError('boom');
    expect(err).toBeInstanceOf(Error);
    expect(err).toBeInstanceOf(NoteBindingMismatchError);
    expect(err.name).toBe('NoteBindingMismatchError');
  });
});
