import {
  AccountId,
  Felt,
  FeltArray,
  Note,
  NoteId,
  NoteRecipient,
  NoteScript,
  NoteStorage,
  Word,
} from '@miden-sdk/miden-sdk';
import type { TransactionSummary } from '@miden-sdk/miden-sdk';

import { getProcedureRoot } from '../procedures.js';
import { deriveP2idSerialNumber } from '../transaction/p2id.js';
import { noteFromBase64, normalizeHexWord } from '../utils/encoding.js';
import {
  isConsumeNotesV2,
  type ConsumeNotesProposalMetadata,
  type P2IdProposalMetadata,
  type ProposalMetadata,
  type UpdateProcedureThresholdProposalMetadata,
  type UpdateSignersProposalMetadata,
} from '../types/proposal.js';

// Multisig auth-component storage slot names (see src/account/masm/auth.ts).
const THRESHOLD_CONFIG_SLOT_NAME = 'openzeppelin::multisig::threshold_config';
const SIGNER_PUBLIC_KEYS_SLOT_NAME = 'openzeppelin::multisig::signer_public_keys';
const PROCEDURE_THRESHOLDS_SLOT_NAME = 'openzeppelin::multisig::procedure_thresholds';

const ZERO_WORD_HEX = normalizeHexWord(`0x${'00'.repeat(32)}`);

function reject(proposalId: string): never {
  throw new Error(`Invalid proposal: metadata does not match tx_summary for ${proposalId}`);
}

function wordFromFelts(felts: bigint[]): string {
  return normalizeHexWord(
    Word.newFromFelts(felts.map((f) => new Felt(f))).toHex(),
  );
}

/**
 * Assert that a proposal's human-readable `metadata` matches the transaction its
 * cosigners actually sign (the `TransactionSummary`), WITHOUT re-executing.
 *
 * The previous implementation rebuilt the transaction from metadata and
 * re-executed it (`executeForSummary`) to compare the resulting summary
 * commitment. That is block-height dependent: execution charges a fee taken from
 * the reference block's fee parameters, and the fee is part of the account delta
 * the summary commitment covers. So the check only matched on the same client at
 * the same sync height as the proposer, and a second signer at a later block hit
 * "metadata does not match tx_summary" and could not load or sign.
 *
 * This instead decodes the signed summary and compares only the intent-bearing
 * components against the metadata. None of those components is the fee (a native
 * -asset vault delta that no check below reads), so the comparison is
 * deterministic across clients and blocks. It preserves the what-you-see-is-what
 * -you-sign guarantee (`docs/MULTISIG_SDK.md`): a mislabeled proposal — one whose
 * metadata does not describe what the signed summary does — is rejected before a
 * cosigner signs it.
 *
 * `switch_guardian` is bound separately by `verifyGuardianEndpointCommitment`,
 * and `custom` proposals carry no metadata recipe; both are exempt here (the
 * id ↔ tx_summary commitment check still applies to them).
 */
export function assertMetadataMatchesSummary(
  proposalId: string,
  metadata: ProposalMetadata,
  summary: TransactionSummary,
): void {
  switch (metadata.proposalType) {
    case 'custom':
    case 'switch_guardian':
      return;
    case 'p2id':
      return assertP2idBinding(proposalId, metadata, summary);
    case 'consume_notes':
      return assertConsumeNotesBinding(proposalId, metadata, summary);
    case 'add_signer':
    case 'remove_signer':
    case 'change_threshold':
      return assertSignerBinding(proposalId, metadata, summary);
    case 'update_procedure_threshold':
      return assertProcedureThresholdBinding(proposalId, metadata, summary);
    default: {
      // Fail closed: a proposal type without a binding recipe must not silently
      // pass. The union is exhaustive, so this is also a compile-time guard that
      // any new built-in type is given an explicit binding (or an explicit
      // exemption above) before it can be signed.
      const _exhaustive: never = metadata;
      void _exhaustive;
      reject(proposalId);
    }
  }
}

/**
 * p2id: the proposal emits exactly one output note. Rebuild that note's
 * recipient from metadata + the SIGNED summary's salt (so a tampered
 * `metadata.saltHex` cannot force a match) and require the summary's single
 * output note to carry that recipient digest and a fungible asset of exactly the
 * declared faucet + amount. The asset is read off the note (not the vault), so
 * the fee never contaminates the comparison.
 */
function assertP2idBinding(
  proposalId: string,
  metadata: P2IdProposalMetadata,
  summary: TransactionSummary,
): void {
  const recipient = AccountId.fromHex(metadata.recipientId);
  const serialNum = deriveP2idSerialNumber(summary.salt());
  const noteRecipient = new NoteRecipient(
    serialNum,
    NoteScript.p2id(),
    new NoteStorage(new FeltArray([recipient.suffix(), recipient.prefix()])),
  );
  const expectedRecipientDigest = normalizeHexWord(noteRecipient.digest().toHex());

  const outputs = summary.outputNotes().notes();
  // Exactly one own output note; extra output notes mean value leaving the
  // account that the metadata does not describe.
  if (outputs.length !== 1) {
    reject(proposalId);
  }
  const note = outputs[0];
  if (normalizeHexWord(note.recipientDigest().toHex()) !== expectedRecipientDigest) {
    reject(proposalId);
  }

  const expectedFaucet = AccountId.fromHex(metadata.faucetId).toString();
  const expectedAmount = BigInt(metadata.amount);
  const asset = note
    .assets()
    ?.fungibleAssets()
    .find((a) => a.faucetId().toString() === expectedFaucet && a.amount() === expectedAmount);
  if (!asset) {
    reject(proposalId);
  }
}

/**
 * consume_notes: the declared note-id set must equal the summary's input-note-id
 * set. Set equality (not membership) so the transaction cannot consume an extra,
 * undeclared note. Note ids are the note commitments (hash of details+metadata)
 * and are stable whether or not the note is authenticated, so this is
 * proof-agnostic.
 */
function assertConsumeNotesBinding(
  proposalId: string,
  metadata: ConsumeNotesProposalMetadata,
  summary: TransactionSummary,
): void {
  const declared = new Set(declaredConsumeNoteIds(metadata));
  const actual = new Set(
    summary
      .inputNotes()
      .notes()
      .map((n) => n.id().toString()),
  );
  if (declared.size !== actual.size) {
    reject(proposalId);
  }
  for (const id of declared) {
    if (!actual.has(id)) {
      reject(proposalId);
    }
  }
}

function declaredConsumeNoteIds(metadata: ConsumeNotesProposalMetadata): string[] {
  if (isConsumeNotesV2(metadata) && metadata.notes) {
    return metadata.notes.map((b64) => noteFromBase64<Note>(b64, Note).id().toString());
  }
  // Canonicalize v1 hex ids through NoteId so they compare equal to the
  // summary's `NoteId.toString()` regardless of casing / 0x padding.
  return metadata.noteIds.map((id) => NoteId.fromHex(id).toString());
}

/**
 * add/remove/change_signer: bind the threshold + signer count exactly (the
 * `[threshold, count, 0, 0]` word written to the threshold-config value slot),
 * and forbid any signer commitment appearing in the public-keys map delta that
 * is not in the declared target set. Together these reject the dangerous
 * mislabels — a changed threshold, a changed signer count, or an unlisted signer
 * (e.g. an attacker) being added — while tolerating the delta-shape differences
 * between add/remove/threshold-only operations (removals appear as zero-value map
 * entries; a threshold-only change leaves the public-keys map untouched). The
 * account's on-chain config-hash check is the backstop for the full signer set.
 */
function assertSignerBinding(
  proposalId: string,
  metadata: UpdateSignersProposalMetadata,
  summary: TransactionSummary,
): void {
  const storage = summary.accountDelta().storage();

  const expectedConfig = wordFromFelts([
    BigInt(metadata.targetThreshold),
    BigInt(metadata.targetSignerCommitments.length),
    0n,
    0n,
  ]);
  const configDelta = storage
    .valueDeltas()
    .find((v) => v.slotName === THRESHOLD_CONFIG_SLOT_NAME);
  if (!configDelta || normalizeHexWord(configDelta.value.toHex()) !== expectedConfig) {
    reject(proposalId);
  }

  const targetCommitments = new Set(
    metadata.targetSignerCommitments.map((c) => normalizeHexWord(c)),
  );
  const pubkeyMap = storage.maps().find((m) => m.slotName === SIGNER_PUBLIC_KEYS_SLOT_NAME);
  if (pubkeyMap) {
    for (const entry of pubkeyMap.entries()) {
      const value = normalizeHexWord(entry.value.toHex());
      // Zero-value entries are removals; every non-zero (added/kept) public key
      // must be one of the declared target signers.
      if (value !== ZERO_WORD_HEX && !targetCommitments.has(value)) {
        reject(proposalId);
      }
    }
  }
}

/**
 * update_procedure_threshold: the procedure-thresholds map delta must set the
 * declared procedure's root to `[threshold, 0, 0, 0]`.
 */
function assertProcedureThresholdBinding(
  proposalId: string,
  metadata: UpdateProcedureThresholdProposalMetadata,
  summary: TransactionSummary,
): void {
  const expectedKey = normalizeHexWord(getProcedureRoot(metadata.targetProcedure));
  const expectedValue = wordFromFelts([BigInt(metadata.targetThreshold), 0n, 0n, 0n]);

  const procMap = summary
    .accountDelta()
    .storage()
    .maps()
    .find((m) => m.slotName === PROCEDURE_THRESHOLDS_SLOT_NAME);
  const entry = procMap
    ?.entries()
    .find((e) => normalizeHexWord(e.key.toHex()) === expectedKey);
  if (!entry || normalizeHexWord(entry.value.toHex()) !== expectedValue) {
    reject(proposalId);
  }
}
