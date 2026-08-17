import { describe, it, expect } from 'vitest';
import {
  AccountId,
  Felt,
  FeltArray,
  NoteId,
  NoteRecipient,
  NoteScript,
  NoteStorage,
  Word,
} from '@miden-sdk/miden-sdk';

import { assertMetadataMatchesSummary } from './summaryBinding.js';
import { deriveP2idSerialNumber } from '../transaction/p2id.js';
import { getProcedureRoot } from '../procedures.js';
import { normalizeHexWord } from '../utils/encoding.js';
import type { ProposalMetadata } from '../types/proposal.js';

const PROPOSAL_ID = '0x' + 'c'.repeat(64);
const SALT = Word.fromHex('0x' + 'ab'.repeat(32));

// Slot names must match src/account/masm/auth.ts.
const THRESHOLD_CONFIG_SLOT = 'openzeppelin::multisig::threshold_config';
const SIGNER_PUBLIC_KEYS_SLOT = 'openzeppelin::multisig::signer_public_keys';
const PROCEDURE_THRESHOLDS_SLOT = 'openzeppelin::multisig::procedure_thresholds';

// Two known-valid Miden account ids (arbitrary hex has an invalid version byte).
const ACCOUNT_1 = '0x69817bcc6fb9f99127c2245f6979c5';
const ACCOUNT_2 = '0x79817bcc6fb9f99127c2245f6979ef';
const FAUCET = AccountId.fromHex(ACCOUNT_2);

interface SummaryParts {
  salt?: Word;
  outputNotes?: unknown[];
  inputNotes?: unknown[];
  valueDeltas?: { slotName: string; valueHex: string }[];
  maps?: { slotName: string; entries: { keyHex: string; valueHex: string }[] }[];
}

function mockSummary(parts: SummaryParts): any {
  return {
    salt: () => parts.salt ?? SALT,
    outputNotes: () => ({ notes: () => parts.outputNotes ?? [] }),
    inputNotes: () => ({ notes: () => parts.inputNotes ?? [] }),
    accountDelta: () => ({
      storage: () => ({
        valueDeltas: () =>
          (parts.valueDeltas ?? []).map((v) => ({
            slotName: v.slotName,
            value: { toHex: () => v.valueHex },
          })),
        maps: () =>
          (parts.maps ?? []).map((m) => ({
            slotName: m.slotName,
            entries: () =>
              m.entries.map((e) => ({
                key: { toHex: () => e.keyHex },
                value: { toHex: () => e.valueHex },
              })),
          })),
      }),
    }),
  };
}

function wordHex(felts: bigint[]): string {
  return normalizeHexWord(Word.newFromFelts(felts.map((f) => new Felt(f))).toHex());
}

function p2idRecipientDigest(recipientId: string, salt: Word): string {
  const recipient = AccountId.fromHex(recipientId);
  const serialNum = deriveP2idSerialNumber(salt);
  const noteRecipient = new NoteRecipient(
    serialNum,
    NoteScript.p2id(),
    new NoteStorage(new FeltArray([recipient.suffix(), recipient.prefix()])),
  );
  return normalizeHexWord(noteRecipient.digest().toHex());
}

function p2idOutputNote(recipientDigestHex: string, faucet: AccountId, amount: bigint): unknown {
  return {
    recipientDigest: () => ({ toHex: () => recipientDigestHex }),
    assets: () => ({
      fungibleAssets: () => [{ faucetId: () => faucet, amount: () => amount }],
    }),
  };
}

function inputNote(idHex: string): unknown {
  return { id: () => ({ toString: () => idHex }) };
}

function expectReject(metadata: ProposalMetadata, summary: any): void {
  expect(() => assertMetadataMatchesSummary(PROPOSAL_ID, metadata, summary)).toThrow(
    `Invalid proposal: metadata does not match tx_summary for ${PROPOSAL_ID}`,
  );
}

function expectPass(metadata: ProposalMetadata, summary: any): void {
  expect(() => assertMetadataMatchesSummary(PROPOSAL_ID, metadata, summary)).not.toThrow();
}

describe('assertMetadataMatchesSummary', () => {
  describe('p2id', () => {
    const recipientId = ACCOUNT_1;
    const faucetId = FAUCET.toString();
    const metadata: ProposalMetadata = {
      proposalType: 'p2id',
      recipientId,
      faucetId,
      amount: '1000',
      description: '',
    };
    const digest = p2idRecipientDigest(recipientId, SALT);

    it('passes when the output note matches recipient, faucet and amount', () => {
      expectPass(metadata, mockSummary({ outputNotes: [p2idOutputNote(digest, FAUCET, 1000n)] }));
    });

    it('rejects a different recipient (mislabel)', () => {
      const wrongDigest = p2idRecipientDigest(ACCOUNT_2, SALT);
      expectReject(metadata, mockSummary({ outputNotes: [p2idOutputNote(wrongDigest, FAUCET, 1000n)] }));
    });

    it('rejects a different amount', () => {
      expectReject(metadata, mockSummary({ outputNotes: [p2idOutputNote(digest, FAUCET, 9999n)] }));
    });

    it('rejects a different faucet', () => {
      const otherFaucet = AccountId.fromHex(ACCOUNT_1);
      expectReject(metadata, mockSummary({ outputNotes: [p2idOutputNote(digest, otherFaucet, 1000n)] }));
    });

    it('rejects an extra output note (unexpected value leaving)', () => {
      const extra = p2idOutputNote(p2idRecipientDigest(ACCOUNT_2, SALT), FAUCET, 1n);
      expectReject(metadata, mockSummary({ outputNotes: [p2idOutputNote(digest, FAUCET, 1000n), extra] }));
    });

    it('rejects when no output note is present', () => {
      expectReject(metadata, mockSummary({ outputNotes: [] }));
    });

    it('rejects a note carrying an extra asset beyond the declared one', () => {
      // Correct recipient + declared asset, but an additional asset would leave
      // the account beyond what the metadata describes.
      const noteWithExtra = {
        recipientDigest: () => ({ toHex: () => digest }),
        assets: () => ({
          fungibleAssets: () => [
            { faucetId: () => FAUCET, amount: () => 1000n },
            { faucetId: () => AccountId.fromHex(ACCOUNT_1), amount: () => 500n },
          ],
        }),
      };
      expectReject(metadata, mockSummary({ outputNotes: [noteWithExtra] }));
    });
  });

  describe('consume_notes (v1)', () => {
    const idA = NoteId.fromHex('0x' + '11'.repeat(32)).toString();
    const idB = NoteId.fromHex('0x' + '22'.repeat(32)).toString();
    const metadata: ProposalMetadata = {
      proposalType: 'consume_notes',
      noteIds: ['0x' + '11'.repeat(32), '0x' + '22'.repeat(32)],
      description: '',
    };

    it('passes on exact set equality', () => {
      expectPass(metadata, mockSummary({ inputNotes: [inputNote(idA), inputNote(idB)] }));
    });

    it('rejects an extra consumed note', () => {
      const idC = NoteId.fromHex('0x' + '33'.repeat(32)).toString();
      expectReject(metadata, mockSummary({ inputNotes: [inputNote(idA), inputNote(idB), inputNote(idC)] }));
    });

    it('rejects a missing declared note', () => {
      expectReject(metadata, mockSummary({ inputNotes: [inputNote(idA)] }));
    });

    it('rejects a substituted note', () => {
      const idC = NoteId.fromHex('0x' + '33'.repeat(32)).toString();
      expectReject(metadata, mockSummary({ inputNotes: [inputNote(idA), inputNote(idC)] }));
    });
  });

  describe('add/remove/change_signer', () => {
    const signers = ['0x' + '1'.repeat(64), '0x' + '2'.repeat(64)];
    const metadata: ProposalMetadata = {
      proposalType: 'add_signer',
      targetThreshold: 2,
      targetSignerCommitments: signers,
      description: '',
    };
    const configHex = wordHex([2n, 2n, 0n, 0n]);

    function pubkeyMap(values: string[]) {
      return [
        {
          slotName: SIGNER_PUBLIC_KEYS_SLOT,
          entries: values.map((v, i) => ({ keyHex: wordHex([BigInt(i), 0n, 0n, 0n]), valueHex: normalizeHexWord(v) })),
        },
      ];
    }

    it('passes on matching threshold/count and target signer set', () => {
      expectPass(
        metadata,
        mockSummary({
          valueDeltas: [{ slotName: THRESHOLD_CONFIG_SLOT, valueHex: configHex }],
          maps: pubkeyMap(signers),
        }),
      );
    });

    it('tolerates a zero-value (removal) map entry', () => {
      expectPass(
        metadata,
        mockSummary({
          valueDeltas: [{ slotName: THRESHOLD_CONFIG_SLOT, valueHex: configHex }],
          maps: [
            {
              slotName: SIGNER_PUBLIC_KEYS_SLOT,
              entries: [
                { keyHex: wordHex([0n, 0n, 0n, 0n]), valueHex: normalizeHexWord(signers[0]) },
                { keyHex: wordHex([1n, 0n, 0n, 0n]), valueHex: normalizeHexWord(signers[1]) },
                { keyHex: wordHex([2n, 0n, 0n, 0n]), valueHex: normalizeHexWord('0x' + '00'.repeat(32)) },
              ],
            },
          ],
        }),
      );
    });

    it('rejects a changed threshold', () => {
      expectReject(
        metadata,
        mockSummary({
          valueDeltas: [{ slotName: THRESHOLD_CONFIG_SLOT, valueHex: wordHex([1n, 2n, 0n, 0n]) }],
          maps: pubkeyMap(signers),
        }),
      );
    });

    it('rejects a changed signer count', () => {
      expectReject(
        metadata,
        mockSummary({
          valueDeltas: [{ slotName: THRESHOLD_CONFIG_SLOT, valueHex: wordHex([2n, 3n, 0n, 0n]) }],
          maps: pubkeyMap(signers),
        }),
      );
    });

    it('rejects an unlisted (attacker) signer in the pubkey map', () => {
      const attacker = '0x' + '9'.repeat(64);
      expectReject(
        metadata,
        mockSummary({
          valueDeltas: [{ slotName: THRESHOLD_CONFIG_SLOT, valueHex: configHex }],
          maps: pubkeyMap([signers[0], attacker]),
        }),
      );
    });

    it('rejects when the threshold-config value slot is absent', () => {
      expectReject(metadata, mockSummary({ valueDeltas: [], maps: pubkeyMap(signers) }));
    });
  });

  describe('update_procedure_threshold', () => {
    const metadata: ProposalMetadata = {
      proposalType: 'update_procedure_threshold',
      targetProcedure: 'send_asset',
      targetThreshold: 3,
      description: '',
    };
    const procRoot = normalizeHexWord(getProcedureRoot('send_asset'));
    const valueHex = wordHex([3n, 0n, 0n, 0n]);

    it('passes when the procedure root maps to the target threshold', () => {
      expectPass(
        metadata,
        mockSummary({ maps: [{ slotName: PROCEDURE_THRESHOLDS_SLOT, entries: [{ keyHex: procRoot, valueHex }] }] }),
      );
    });

    it('rejects a different threshold value', () => {
      expectReject(
        metadata,
        mockSummary({
          maps: [{ slotName: PROCEDURE_THRESHOLDS_SLOT, entries: [{ keyHex: procRoot, valueHex: wordHex([1n, 0n, 0n, 0n]) }] }],
        }),
      );
    });

    it('rejects when the declared procedure is not in the delta', () => {
      const otherRoot = normalizeHexWord(getProcedureRoot('receive_asset'));
      expectReject(
        metadata,
        mockSummary({ maps: [{ slotName: PROCEDURE_THRESHOLDS_SLOT, entries: [{ keyHex: otherRoot, valueHex }] }] }),
      );
    });
  });

  describe('exempt types', () => {
    it('never rejects custom proposals', () => {
      expectPass({ proposalType: 'custom', rawProposalType: 'x', description: '' } as ProposalMetadata, mockSummary({}));
    });

    it('never rejects switch_guardian proposals', () => {
      expectPass(
        { proposalType: 'switch_guardian', newGuardianPubkey: '0x' + '1'.repeat(64), description: '' } as ProposalMetadata,
        mockSummary({}),
      );
    });
  });
});
