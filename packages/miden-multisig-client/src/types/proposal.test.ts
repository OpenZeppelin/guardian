import { describe, expect, it } from 'vitest';
import {
  CONSUME_NOTES_METADATA_VERSION_V2,
  type ConsumeNotesProposalMetadata,
  MAX_CONSUME_NOTES_METADATA_BYTES,
  isConsumeNotesV1,
  isConsumeNotesV2,
} from './proposal.js';

describe('consume_notes metadata v1/v2 discriminator (issue #229)', () => {
  it('v1 metadata omits metadataVersion and notes on the wire', () => {
    const md: ConsumeNotesProposalMetadata = {
      proposalType: 'consume_notes',
      description: '',
      noteIds: ['0xabc', '0xdef'],
    };

    const json = JSON.parse(JSON.stringify(md));
    expect(json.metadataVersion).toBeUndefined();
    expect(json.notes).toBeUndefined();
    expect(isConsumeNotesV1(md)).toBe(true);
    expect(isConsumeNotesV2(md)).toBe(false);
  });

  it('v2 metadata round-trips with discriminator and embedded notes', () => {
    const md: ConsumeNotesProposalMetadata = {
      proposalType: 'consume_notes',
      description: '',
      noteIds: ['0xabc'],
      metadataVersion: CONSUME_NOTES_METADATA_VERSION_V2,
      notes: ['YmFzZTY0Tm90ZQ=='],
    };

    const json = JSON.stringify(md);
    const parsed = JSON.parse(json) as ConsumeNotesProposalMetadata;
    expect(parsed.metadataVersion).toBe(2);
    expect(parsed.notes).toEqual(['YmFzZTY0Tm90ZQ==']);
    expect(isConsumeNotesV2(parsed)).toBe(true);
    expect(isConsumeNotesV1(parsed)).toBe(false);
  });

  it('explicit metadataVersion === 1 is treated as v1', () => {
    const md = {
      proposalType: 'consume_notes',
      description: '',
      noteIds: ['0xabc'],
      metadataVersion: 1 as unknown as 2, // legacy explicit-v1 case
    } as ConsumeNotesProposalMetadata;
    expect(isConsumeNotesV1(md)).toBe(true);
    expect(isConsumeNotesV2(md)).toBe(false);
  });

  it('unknown future version is neither v1 nor v2; dispatch handles rejection', () => {
    const md = {
      proposalType: 'consume_notes',
      description: '',
      noteIds: ['0xabc'],
      metadataVersion: 99 as unknown as 2,
    } as ConsumeNotesProposalMetadata;
    expect(isConsumeNotesV1(md)).toBe(false);
    expect(isConsumeNotesV2(md)).toBe(false);
  });

  it('pins MAX_CONSUME_NOTES_METADATA_BYTES at 256 KiB', () => {
    expect(MAX_CONSUME_NOTES_METADATA_BYTES).toBe(256 * 1024);
    expect(MAX_CONSUME_NOTES_METADATA_BYTES).toBe(262_144);
  });
});
