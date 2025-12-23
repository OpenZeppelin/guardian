import { describe, it, expect } from 'vitest';
import { fromPsmMetadata, toPsmMetadata } from './metadata.js';

describe('metadata conversion', () => {
  describe('fromPsmMetadata', () => {
    it('returns undefined for null/undefined input', () => {
      expect(fromPsmMetadata(undefined)).toBeUndefined();
      expect(fromPsmMetadata(null as any)).toBeUndefined();
    });

    it('converts P2ID metadata from PSM shape', () => {
      const raw = {
        proposalType: 'p2id',
        recipientId: '0xabc',
        faucetId: '0xf00',
        amount: '42',
        description: 'send funds',
        saltHex: '0xsalt',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toEqual({
        proposalType: 'p2id',
        recipientId: '0xabc',
        faucetId: '0xf00',
        amount: '42',
        description: 'send funds',
        saltHex: '0xsalt',
      });
    });

    it('converts consume_notes metadata', () => {
      const raw = {
        proposalType: 'consume_notes',
        noteIds: ['0xnote1', '0xnote2'],
        description: 'consume notes',
        saltHex: '0xsalt',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toEqual({
        proposalType: 'consume_notes',
        noteIds: ['0xnote1', '0xnote2'],
        description: 'consume notes',
        saltHex: '0xsalt',
      });
    });

    it('converts switch_psm metadata', () => {
      const raw = {
        proposalType: 'switch_psm',
        newPsmPubkey: '0xpubkey',
        newPsmEndpoint: 'http://new-psm.com',
        description: 'switch PSM',
        saltHex: '0xsalt',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toEqual({
        proposalType: 'switch_psm',
        newPsmPubkey: '0xpubkey',
        newPsmEndpoint: 'http://new-psm.com',
        description: 'switch PSM',
        saltHex: '0xsalt',
      });
    });

    it('converts add_signer metadata', () => {
      const raw = {
        proposalType: 'add_signer',
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        description: 'add signer',
        saltHex: '0xsalt',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toEqual({
        proposalType: 'add_signer',
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        description: 'add signer',
        saltHex: '0xsalt',
      });
    });

    it('converts remove_signer metadata', () => {
      const raw = {
        proposalType: 'remove_signer',
        targetThreshold: 1,
        targetSignerCommitments: ['0x1'],
        description: 'remove signer',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toEqual({
        proposalType: 'remove_signer',
        targetThreshold: 1,
        targetSignerCommitments: ['0x1'],
        description: 'remove signer',
        saltHex: undefined,
      });
    });

    it('converts change_threshold metadata', () => {
      const raw = {
        proposalType: 'change_threshold',
        targetThreshold: 3,
        targetSignerCommitments: ['0x1', '0x2', '0x3'],
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toEqual({
        proposalType: 'change_threshold',
        targetThreshold: 3,
        targetSignerCommitments: ['0x1', '0x2', '0x3'],
        description: '',
        saltHex: undefined,
      });
    });

    it('infers p2id from recipientId field', () => {
      const raw = {
        recipientId: '0xabc',
        faucetId: '0xf00',
        amount: '42',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta?.proposalType).toBe('p2id');
    });

    it('infers p2id from faucetId field', () => {
      const raw = {
        faucetId: '0xf00',
        amount: '42',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta?.proposalType).toBe('p2id');
    });

    it('infers p2id from amount field', () => {
      const raw = {
        amount: '42',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta?.proposalType).toBe('p2id');
    });

    it('infers consume_notes from noteIds field', () => {
      const raw = {
        noteIds: ['0xnote1'],
      };

      const meta = fromPsmMetadata(raw);

      expect(meta?.proposalType).toBe('consume_notes');
    });

    it('infers switch_psm from newPsmPubkey field', () => {
      const raw = {
        newPsmPubkey: '0xpubkey',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta?.proposalType).toBe('switch_psm');
    });

    it('infers change_threshold from targetSignerCommitments field', () => {
      const raw = {
        targetSignerCommitments: ['0x1', '0x2'],
        targetThreshold: 2,
      };

      const meta = fromPsmMetadata(raw);

      expect(meta?.proposalType).toBe('change_threshold');
    });

    it('returns undefined when proposalType cannot be inferred', () => {
      const raw = {
        unknownField: 'value',
      };

      const meta = fromPsmMetadata(raw as any);

      expect(meta).toBeUndefined();
    });

    it('returns undefined for empty noteIds array', () => {
      const raw = {
        noteIds: [],
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toBeUndefined();
    });

    it('defaults missing optional fields', () => {
      const raw = {
        proposalType: 'p2id',
      };

      const meta = fromPsmMetadata(raw);

      expect(meta).toEqual({
        proposalType: 'p2id',
        recipientId: '',
        faucetId: '',
        amount: '0',
        description: '',
        saltHex: undefined,
      });
    });
  });

  describe('toPsmMetadata', () => {
    it('returns undefined for undefined input', () => {
      expect(toPsmMetadata(undefined)).toBeUndefined();
    });

    it('maps add_signer to PSM metadata', () => {
      const meta = toPsmMetadata({
        proposalType: 'add_signer',
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        saltHex: '0xsalt',
        description: 'add signer',
      });

      expect(meta).toEqual({
        proposalType: 'add_signer',
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        saltHex: '0xsalt',
        description: 'add signer',
      });
    });

    it('maps remove_signer to PSM metadata', () => {
      const meta = toPsmMetadata({
        proposalType: 'remove_signer',
        targetThreshold: 1,
        targetSignerCommitments: ['0x1'],
        description: '',
      });

      expect(meta).toEqual({
        proposalType: 'remove_signer',
        targetThreshold: 1,
        targetSignerCommitments: ['0x1'],
        saltHex: undefined,
        description: '',
      });
    });

    it('maps change_threshold to PSM metadata', () => {
      const meta = toPsmMetadata({
        proposalType: 'change_threshold',
        targetThreshold: 3,
        targetSignerCommitments: ['0x1', '0x2', '0x3'],
        description: '',
      });

      expect(meta).toEqual({
        proposalType: 'change_threshold',
        targetThreshold: 3,
        targetSignerCommitments: ['0x1', '0x2', '0x3'],
        saltHex: undefined,
        description: '',
      });
    });

    it('maps p2id to PSM metadata', () => {
      const meta = toPsmMetadata({
        proposalType: 'p2id',
        recipientId: '0xabc',
        faucetId: '0xf00',
        amount: '42',
        description: 'send funds',
        saltHex: '0xsalt',
      });

      expect(meta).toEqual({
        proposalType: 'p2id',
        recipientId: '0xabc',
        faucetId: '0xf00',
        amount: '42',
        description: 'send funds',
        saltHex: '0xsalt',
      });
    });

    it('maps consume_notes to PSM metadata', () => {
      const meta = toPsmMetadata({
        proposalType: 'consume_notes',
        noteIds: ['0xnote1', '0xnote2'],
        description: 'consume',
      });

      expect(meta).toEqual({
        proposalType: 'consume_notes',
        noteIds: ['0xnote1', '0xnote2'],
        description: 'consume',
        saltHex: undefined,
      });
    });

    it('maps switch_psm to PSM metadata', () => {
      const meta = toPsmMetadata({
        proposalType: 'switch_psm',
        newPsmPubkey: '0xpubkey',
        newPsmEndpoint: 'http://new.com',
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        description: '',
      });

      expect(meta).toEqual({
        proposalType: 'switch_psm',
        newPsmPubkey: '0xpubkey',
        newPsmEndpoint: 'http://new.com',
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        description: '',
        saltHex: undefined,
      });
    });
  });

  describe('roundtrip', () => {
    it('p2id metadata survives roundtrip', () => {
      const original = {
        proposalType: 'p2id' as const,
        recipientId: '0xabc',
        faucetId: '0xf00',
        amount: '42',
        description: 'test',
        saltHex: '0xsalt',
      };

      const psm = toPsmMetadata(original);
      const roundtripped = fromPsmMetadata(psm as any);

      expect(roundtripped).toEqual(original);
    });

    it('consume_notes metadata survives roundtrip', () => {
      const original = {
        proposalType: 'consume_notes' as const,
        noteIds: ['0xnote1', '0xnote2'],
        description: 'test',
        saltHex: '0xsalt',
      };

      const psm = toPsmMetadata(original);
      const roundtripped = fromPsmMetadata(psm as any);

      expect(roundtripped).toEqual(original);
    });

    it('add_signer metadata survives roundtrip', () => {
      const original = {
        proposalType: 'add_signer' as const,
        targetThreshold: 2,
        targetSignerCommitments: ['0x1', '0x2'],
        description: 'test',
        saltHex: '0xsalt',
      };

      const psm = toPsmMetadata(original);
      const roundtripped = fromPsmMetadata(psm as any);

      expect(roundtripped).toEqual(original);
    });
  });
});
