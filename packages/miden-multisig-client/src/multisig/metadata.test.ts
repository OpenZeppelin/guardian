import { describe, it, expect } from 'vitest';
import { fromPsmMetadata, toPsmMetadata } from './metadata.js';

describe('metadata conversion', () => {
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
      kind: 'p2id',
      recipientId: '0xabc',
      faucetId: '0xf00',
      amount: '42',
      description: 'send funds',
      saltHex: '0xsalt',
    });
  });

  it('maps signer updates to PSM metadata', () => {
    const meta = toPsmMetadata({
      kind: 'add_signer',
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
});

