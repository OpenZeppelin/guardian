import { describe, expect, it } from 'vitest';
import type { ProposalMetadata as GuardianProposalMetadata } from '@openzeppelin/guardian-client';
import { ProposalMetadataCodec } from './metadata.js';
import type { ConsumeNotesProposalMetadata } from '../types/proposal.js';

describe('ProposalMetadataCodec consume_notes v2 round-trip (issue #229)', () => {
  it('toGuardian threads metadataVersion and notes to the wire', () => {
    const md: ConsumeNotesProposalMetadata = {
      proposalType: 'consume_notes',
      description: 'consume one note',
      noteIds: ['0xabc'],
      metadataVersion: 2,
      notes: ['YmFzZTY0Tm90ZQ=='],
    };
    const wire = ProposalMetadataCodec.toGuardian(md);
    expect(wire.noteIds).toEqual(['0xabc']);
    expect(wire.consumeNotesMetadataVersion).toBe(2);
    expect(wire.consumeNotesNotes).toEqual(['YmFzZTY0Tm90ZQ==']);
  });

  it('fromGuardian reconstructs the v2 fields', () => {
    const wire: GuardianProposalMetadata = {
      proposalType: 'consume_notes',
      noteIds: ['0xabc'],
      consumeNotesMetadataVersion: 2,
      consumeNotesNotes: ['YmFzZTY0Tm90ZQ=='],
    };
    const md = ProposalMetadataCodec.fromGuardian(wire) as ConsumeNotesProposalMetadata;
    expect(md.proposalType).toBe('consume_notes');
    expect(md.metadataVersion).toBe(2);
    expect(md.notes).toEqual(['YmFzZTY0Tm90ZQ==']);
  });

  it('round-trips a v1 (legacy) proposal without spurious v2 fields', () => {
    const md: ConsumeNotesProposalMetadata = {
      proposalType: 'consume_notes',
      description: 'legacy',
      noteIds: ['0xabc'],
    };
    const wire = ProposalMetadataCodec.toGuardian(md);
    expect(wire.consumeNotesMetadataVersion).toBeUndefined();
    expect(wire.consumeNotesNotes).toBeUndefined();
    const back = ProposalMetadataCodec.fromGuardian(wire) as ConsumeNotesProposalMetadata;
    expect(back.metadataVersion).toBeUndefined();
    expect(back.notes).toBeUndefined();
  });
});
