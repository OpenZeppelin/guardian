import { describe, expect, it } from 'vitest';
import type { ProposalMetadata as GuardianProposalMetadata } from '@openzeppelin/guardian-client';
import { ProposalMetadataCodec } from './metadata.js';
import type {
  ConsumeNotesProposalMetadata,
  CustomProposalMetadata,
  P2IdProposalMetadata,
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

describe('ProposalMetadataCodec p2id noteType (issue #322)', () => {
  const baseWire: GuardianProposalMetadata = {
    proposalType: 'p2id',
    recipientId: '0xrecipient',
    faucetId: '0xfaucet',
    amount: '1000',
  };

  it('round-trips a private noteType through the codec', () => {
    const md = ProposalMetadataCodec.fromGuardian({
      ...baseWire,
      noteType: 'private',
    }) as P2IdProposalMetadata;
    expect(md.noteType).toBe('private');

    const wire = ProposalMetadataCodec.toGuardian(md);
    expect(wire.noteType).toBe('private');
  });

  it('leaves noteType absent for legacy proposals (=> public)', () => {
    const md = ProposalMetadataCodec.fromGuardian(baseWire) as P2IdProposalMetadata;
    expect(md.noteType).toBeUndefined();
    expect(ProposalMetadataCodec.toGuardian(md).noteType).toBeUndefined();
  });

  it('canonicalizes an explicit public noteType to absent on encode', () => {
    const md = {
      proposalType: 'p2id',
      description: '',
      recipientId: '0xrecipient',
      faucetId: '0xfaucet',
      amount: '1000',
      noteType: 'public',
    } as P2IdProposalMetadata;

    // toGuardian omits the field so a public note keeps the pre-#322 wire
    // shape and matches the Rust encoder, even if handed an explicit 'public'.
    expect(ProposalMetadataCodec.toGuardian(md).noteType).toBeUndefined();
  });

  it('fromGuardian rejects an unsupported noteType', () => {
    expect(() =>
      ProposalMetadataCodec.fromGuardian({ ...baseWire, noteType: 'encrypted' }),
    ).toThrow(/unsupported noteType/);
  });

  it('validate rejects an unsupported noteType', () => {
    const md = {
      proposalType: 'p2id',
      description: '',
      recipientId: '0xrecipient',
      faucetId: '0xfaucet',
      amount: '1000',
      noteType: 'encrypted',
    } as unknown as P2IdProposalMetadata;
    expect(() => ProposalMetadataCodec.validate(md)).toThrow(/unsupported noteType/);
  });
});
