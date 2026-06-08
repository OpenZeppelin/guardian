import { describe, expect, it } from 'vitest';
import type { ProposalMetadata as GuardianProposalMetadata } from '@openzeppelin/guardian-client';
import { ProposalMetadataCodec } from './metadata.js';
import type {
  ConsumeNotesProposalMetadata,
  CustomProposalMetadata,
} from '../types/proposal.js';

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

describe('ProposalMetadataCodec custom proposal types (issue #266)', () => {
  it('fromGuardian collapses an unmodeled type to the custom bucket, keeping the raw label', () => {
    const wire: GuardianProposalMetadata = {
      proposalType: 'b2agg',
      description: 'agglayer bridge note',
    };
    const md = ProposalMetadataCodec.fromGuardian(wire) as CustomProposalMetadata;
    expect(md.proposalType).toBe('custom');
    expect(md.rawProposalType).toBe('b2agg');
  });

  it('toGuardian round-trips the raw label, not the custom bucket', () => {
    const md: CustomProposalMetadata = {
      proposalType: 'custom',
      description: 'agglayer bridge note',
      rawProposalType: 'b2agg',
    };
    const wire = ProposalMetadataCodec.toGuardian(md);
    expect(wire.proposalType).toBe('b2agg');

    const back = ProposalMetadataCodec.fromGuardian(wire) as CustomProposalMetadata;
    expect(back.proposalType).toBe('custom');
    expect(back.rawProposalType).toBe('b2agg');
  });

  it('validate accepts a custom proposal', () => {
    const md: CustomProposalMetadata = {
      proposalType: 'custom',
      description: 'opaque',
      rawProposalType: 'b2agg',
    };
    expect(ProposalMetadataCodec.validate(md)).toBe(md);
  });

  it('round-trips update_procedure_threshold through the codec', () => {
    const wire: GuardianProposalMetadata = {
      proposalType: 'update_procedure_threshold',
      targetProcedure: 'send_asset',
      targetThreshold: 2,
    };
    const md = ProposalMetadataCodec.fromGuardian(wire);
    expect(md.proposalType).toBe('update_procedure_threshold');
    const back = ProposalMetadataCodec.toGuardian(md);
    expect(back.proposalType).toBe('update_procedure_threshold');
    expect(back.targetProcedure).toBe('send_asset');
    expect(back.targetThreshold).toBe(2);
  });
});
