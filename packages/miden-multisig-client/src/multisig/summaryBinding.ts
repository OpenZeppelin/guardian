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
  type SwitchGuardianProposalMetadata,
  type UpdateProcedureThresholdProposalMetadata,
  type UpdateSignersProposalMetadata,
} from '../types/proposal.js';

// Storage slot names (see src/account/masm/auth.ts).
const THRESHOLD_CONFIG_SLOT = 'openzeppelin::multisig::threshold_config';
const SIGNER_PUBLIC_KEYS_SLOT = 'openzeppelin::multisig::signer_public_keys';
const SIGNER_SCHEME_IDS_SLOT = 'openzeppelin::multisig::signer_scheme_ids';
const PROCEDURE_THRESHOLDS_SLOT = 'openzeppelin::multisig::procedure_thresholds';
// Every multisig transaction writes this map (a per-tx replay marker), so it is
// always an allowed storage slot.
const EXECUTED_TXS_SLOT = 'openzeppelin::multisig::executed_transactions';
const GUARDIAN_SELECTOR_SLOT = 'openzeppelin::guardian::selector';
const GUARDIAN_PUBLIC_KEY_SLOT = 'openzeppelin::guardian::public_key';
const GUARDIAN_SCHEME_ID_SLOT = 'openzeppelin::guardian::scheme_id';

const ZERO_WORD_HEX = normalizeHexWord(`0x${'00'.repeat(32)}`);

function reject(proposalId: string): never {
  throw new Error(`Invalid proposal: metadata does not match tx_summary for ${proposalId}`);
}

function wordHexFromFelts(felts: bigint[]): string {
  return normalizeHexWord(Word.newFromFelts(felts.map((f) => new Felt(f))).toHex());
}

// A shared view of the storage delta, plus the slot-scoping helpers used by every
// per-type check to prove the transaction touches ONLY the slots that type may.
type StorageDelta = ReturnType<ReturnType<TransactionSummary['accountDelta']>['storage']>;

function storageOf(summary: TransactionSummary): StorageDelta {
  return summary.accountDelta().storage();
}

/** No output notes: nothing leaves the account (beyond the fee, a vault delta). */
function assertNoOutputNotes(proposalId: string, summary: TransactionSummary): void {
  if (summary.outputNotes().numNotes() !== 0) {
    reject(proposalId);
  }
}

/** No input notes: the transaction consumes nothing. */
function assertNoInputNotes(proposalId: string, summary: TransactionSummary): void {
  if (summary.inputNotes().numNotes() !== 0) {
    reject(proposalId);
  }
}

/**
 * Every storage slot the transaction changed (value slots and map slots) must be
 * in `allowed`. This is the load-bearing exhaustiveness check: it stops an
 * attacker from piggybacking an undeclared storage effect (e.g. rewriting the
 * signer set inside a "p2id" proposal).
 */
function assertStorageSlotsWithin(
  proposalId: string,
  storage: StorageDelta,
  allowed: ReadonlySet<string>,
): void {
  for (const v of storage.valueDeltas()) {
    if (!allowed.has(v.slotName)) {
      reject(proposalId);
    }
  }
  for (const m of storage.maps()) {
    if (!allowed.has(m.slotName)) {
      reject(proposalId);
    }
  }
}

function valueDeltaFor(storage: StorageDelta, slotName: string): string | undefined {
  const delta = storage.valueDeltas().find((v) => v.slotName === slotName);
  return delta ? normalizeHexWord(delta.value.toHex()) : undefined;
}

function mapEntriesFor(
  storage: StorageDelta,
  slotName: string,
): Array<{ key: string; value: string }> {
  const map = storage.maps().find((m) => m.slotName === slotName);
  if (!map) {
    return [];
  }
  return map.entries().map((e) => ({
    key: normalizeHexWord(e.key.toHex()),
    value: normalizeHexWord(e.value.toHex()),
  }));
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
 * This instead decodes the signed summary and, for each type, asserts the
 * transaction's effects are EXACTLY the declared effect plus the fee — across
 * every intent-bearing dimension of the summary: its output notes, its input
 * notes, and the set of account-storage slots it changes. It never reads the
 * vault delta (the fee is a native-asset vault delta, block-dependent), which is
 * safe because assets can only leave the account through an output note or the
 * fee: binding the output/input notes and the storage slots exactly makes the
 * vault delta a consequence. So the comparison is deterministic across clients
 * and blocks yet still what-you-see-is-what-you-sign — a mislabeled proposal, or
 * one that piggybacks an undeclared effect in any dimension, is rejected before a
 * cosigner signs it.
 *
 * Residual limitations (see the type comments): the SDK exposes only FUNGIBLE
 * note/vault assets, so a non-fungible asset attached to an otherwise-correct
 * p2id note cannot be detected here; and the signer check binds by storage-map
 * index over the CHANGED entries (a real-summary integration test should confirm
 * the delta carries every changed index). `custom` proposals carry no metadata
 * recipe, so WYSIWYS does not hold for them — a cosigner must verify the raw
 * `tx_summary`, never trust a `custom` proposal's `description`.
 */
export function assertMetadataMatchesSummary(
  proposalId: string,
  metadata: ProposalMetadata,
  summary: TransactionSummary,
): void {
  switch (metadata.proposalType) {
    case 'custom':
      // No reconstruction recipe; the id <-> tx_summary commitment check is the
      // only guarantee. WYSIWYS does NOT hold for custom proposals.
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
    case 'switch_guardian':
      return assertSwitchGuardianBinding(proposalId, metadata, summary);
    default: {
      // Fail closed: a proposal type without a binding recipe must not silently
      // pass. The union is exhaustive, so this is also a compile-time guard that
      // any new built-in type is given an explicit binding before it can be
      // signed.
      const _exhaustive: never = metadata;
      void _exhaustive;
      reject(proposalId);
    }
  }
}

/**
 * p2id: exactly one output note carrying the declared recipient and exactly the
 * declared fungible asset; no input notes; no storage change other than the
 * per-tx executed-transactions marker. The recipient is rebuilt from metadata +
 * the SIGNED summary's salt (so a tampered `metadata.saltHex` cannot force a
 * match); the asset is read off the note (not the vault), so the block-dependent
 * fee never contaminates the comparison.
 *
 * Residual: `NoteAssets` exposes only fungible assets, so a non-fungible asset
 * attached to this note is invisible to the SDK and cannot be bound here.
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
  if (outputs.length !== 1) {
    reject(proposalId);
  }
  const note = outputs[0];
  if (normalizeHexWord(note.recipientDigest().toHex()) !== expectedRecipientDigest) {
    reject(proposalId);
  }

  const expectedFaucet = AccountId.fromHex(metadata.faucetId).toString();
  const expectedAmount = BigInt(metadata.amount);
  const assets = note.assets()?.fungibleAssets() ?? [];
  if (
    assets.length !== 1 ||
    assets[0].faucetId().toString() !== expectedFaucet ||
    assets[0].amount() !== expectedAmount
  ) {
    reject(proposalId);
  }

  assertNoInputNotes(proposalId, summary);
  assertStorageSlotsWithin(proposalId, storageOf(summary), new Set([EXECUTED_TXS_SLOT]));
}

/**
 * consume_notes: input-note-id set exactly equals the declared set; NO output
 * notes (a consume that also emitted an output note would drain value the
 * metadata does not describe); no storage change other than the executed-tx
 * marker. Note ids are content commitments, stable across authentication, so the
 * comparison is proof-agnostic.
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

  assertNoOutputNotes(proposalId, summary);
  assertStorageSlotsWithin(proposalId, storageOf(summary), new Set([EXECUTED_TXS_SLOT]));
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
 * add/remove/change_signer: no notes; the only value slot changed is
 * `threshold_config` set to `[threshold, count, 0, 0]`; the only map slots
 * changed are the signer public-keys / scheme-ids (and the executed-tx marker);
 * and every changed public-keys entry binds by INDEX to the declared signer set.
 *
 * The MASM writes signer `j` to map key `[j,0,0,0]` (auth.ts
 * `update_signers_and_threshold`), and `cleanup_pubkey_mapping` zeroes stale
 * higher indices. So a changed entry at index `j` must equal the declared
 * commitment at `j` (or be a zero-value removal at an index >= count). This is
 * stronger than a subset check: it catches a duplicated signer (same key at two
 * indices) or an omitted one. Residual: the storage delta carries only CHANGED
 * indices, so an unchanged index is not re-verified here — a real-summary
 * integration test should confirm the delta covers every changed index.
 */
function assertSignerBinding(
  proposalId: string,
  metadata: UpdateSignersProposalMetadata,
  summary: TransactionSummary,
): void {
  assertNoOutputNotes(proposalId, summary);
  assertNoInputNotes(proposalId, summary);

  const storage = storageOf(summary);
  assertStorageSlotsWithin(
    proposalId,
    storage,
    new Set([THRESHOLD_CONFIG_SLOT, SIGNER_PUBLIC_KEYS_SLOT, SIGNER_SCHEME_IDS_SLOT, EXECUTED_TXS_SLOT]),
  );
  assertSignerConfigAndKeys(proposalId, storage, metadata.targetThreshold, metadata.targetSignerCommitments);
}

/**
 * Shared signer-config check: the `threshold_config` value slot equals
 * `[threshold, count, 0, 0]`, and each changed `signer_public_keys` entry binds
 * by index to the declared commitment set. Used by the signer bindings and by a
 * `switch_guardian` proposal that also rotates the signer set.
 */
function assertSignerConfigAndKeys(
  proposalId: string,
  storage: StorageDelta,
  targetThreshold: number,
  targetSignerCommitments: string[],
): void {
  const expectedConfig = wordHexFromFelts([
    BigInt(targetThreshold),
    BigInt(targetSignerCommitments.length),
    0n,
    0n,
  ]);
  if (valueDeltaFor(storage, THRESHOLD_CONFIG_SLOT) !== expectedConfig) {
    reject(proposalId);
  }

  // Expected value at each map index key [i,0,0,0].
  const expectedByKey = new Map<string, string>();
  targetSignerCommitments.forEach((commitment, i) => {
    expectedByKey.set(wordHexFromFelts([BigInt(i), 0n, 0n, 0n]), normalizeHexWord(commitment));
  });

  for (const entry of mapEntriesFor(storage, SIGNER_PUBLIC_KEYS_SLOT)) {
    const expected = expectedByKey.get(entry.key);
    if (expected !== undefined) {
      // A declared index: the written key must be exactly the declared signer.
      if (entry.value !== expected) {
        reject(proposalId);
      }
    } else if (entry.value !== ZERO_WORD_HEX) {
      // An index outside the declared set may only be a zero-value removal.
      reject(proposalId);
    }
  }
}

/**
 * update_procedure_threshold: no notes; no value-slot change; the only map slots
 * changed are `procedure_thresholds` (and the executed-tx marker), and it must
 * set the declared procedure's root to `[threshold, 0, 0, 0]`.
 */
function assertProcedureThresholdBinding(
  proposalId: string,
  metadata: UpdateProcedureThresholdProposalMetadata,
  summary: TransactionSummary,
): void {
  assertNoOutputNotes(proposalId, summary);
  assertNoInputNotes(proposalId, summary);

  const storage = storageOf(summary);
  assertStorageSlotsWithin(
    proposalId,
    storage,
    new Set([PROCEDURE_THRESHOLDS_SLOT, EXECUTED_TXS_SLOT]),
  );

  const expectedKey = normalizeHexWord(getProcedureRoot(metadata.targetProcedure));
  const expectedValue = wordHexFromFelts([BigInt(metadata.targetThreshold), 0n, 0n, 0n]);
  const entry = mapEntriesFor(storage, PROCEDURE_THRESHOLDS_SLOT).find((e) => e.key === expectedKey);
  if (!entry || entry.value !== expectedValue) {
    reject(proposalId);
  }
}

/**
 * switch_guardian: no notes; the guardian public-key map is set to the declared
 * new key (`[0,0,0,0] => newGuardianPubkey`, auth.ts `update_guardian_public_key`);
 * and the changed storage slots are confined to the guardian component (plus the
 * executed-tx marker) — and, if the proposal also rotates the signer set, the
 * multisig signer slots with the same index-bound checks. `verifyGuardianEndpoint
 * Commitment` (propose/execute) remains an additional check that the new key is
 * live at the declared endpoint.
 */
function assertSwitchGuardianBinding(
  proposalId: string,
  metadata: SwitchGuardianProposalMetadata,
  summary: TransactionSummary,
): void {
  assertNoOutputNotes(proposalId, summary);
  assertNoInputNotes(proposalId, summary);

  const storage = storageOf(summary);
  const allowed = new Set([
    GUARDIAN_SELECTOR_SLOT,
    GUARDIAN_PUBLIC_KEY_SLOT,
    GUARDIAN_SCHEME_ID_SLOT,
    EXECUTED_TXS_SLOT,
  ]);
  const rotatesSigners =
    metadata.targetSignerCommitments !== undefined && metadata.targetThreshold !== undefined;
  if (rotatesSigners) {
    allowed.add(THRESHOLD_CONFIG_SLOT);
    allowed.add(SIGNER_PUBLIC_KEYS_SLOT);
    allowed.add(SIGNER_SCHEME_IDS_SLOT);
  }
  assertStorageSlotsWithin(proposalId, storage, allowed);

  // The guardian public-key map is keyed by the zero word.
  const expectedPubkey = normalizeHexWord(metadata.newGuardianPubkey);
  const entry = mapEntriesFor(storage, GUARDIAN_PUBLIC_KEY_SLOT).find((e) => e.key === ZERO_WORD_HEX);
  if (!entry || entry.value !== expectedPubkey) {
    reject(proposalId);
  }

  if (rotatesSigners) {
    assertSignerConfigAndKeys(
      proposalId,
      storage,
      metadata.targetThreshold as number,
      metadata.targetSignerCommitments as string[],
    );
  }
}
