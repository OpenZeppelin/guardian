import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';

import { GuardianHttpClient } from '@openzeppelin/guardian-client';

import { AccountInspector } from '../../../src/inspector.js';
import { AccountId, FeltArray, Poseidon2, TransactionSummary, Word } from '@miden-sdk/miden-sdk';

import { computeCommitmentFromTxSummary } from '../../../src/multisig/helpers.js';
import { ProposalMetadataCodec } from '../../../src/proposal/metadata.js';
import { buildP2idTransactionRequest } from '../../../src/transaction.js';
import {
  base64ToUint8Array,
  bytesToHex,
  normalizeHexWord as normalizeWord,
} from '../../../src/utils/encoding.js';
import { cosignWithRust } from '../handoff.js';
import { fundAccount } from '../funding.js';
import {
  buildCosigners,
  guardianCommitment,
  shapeOf,
  type LiveSession,
  type Scheme,
} from '../live.js';
import { setAccountPaused } from './operator.js';
import type { ActionContext, ActionOutcome } from '../runner.js';

/** Matches the Rust driver, so the two legs fund identically. */
const ACCOUNT_FUNDING = 200_000;
const NOTE_ARRIVAL_DEADLINE_MS = 180_000;
// Chain events usually land within a few seconds, so early polls are tight and
// back off rather than waiting a flat interval every time.
const POLL_START_MS = 1_000;
const POLL_MAX_MS = 5_000;

async function backoff(current: number): Promise<number> {
  await new Promise((resolve) => setTimeout(resolve, current));
  return Math.min(current * 2, POLL_MAX_MS);
}
const CANONICALIZATION_DEADLINE_MS = 180_000;

const sessions = new Map<string, LiveSession>();

export function resetSession(scenarioId: string): void {
  sessions.delete(scenarioId);
}

function requireLive(context: ActionContext): ActionOutcome | null {
  if (!context.live) {
    return { kind: 'failed', classification: 'setup', reason: 'the live context is not configured' };
  }
  return null;
}

export async function createAccount(
  context: ActionContext,
  scenarioId: string,
  shape: string,
  scheme: Scheme,
): Promise<ActionOutcome> {
  const missing = requireLive(context);
  if (missing) return missing;

  const parsed = shapeOf(shape);
  if (!parsed) {
    return { kind: 'failed', classification: 'setup', reason: `${shape} is not a usable multisig shape` };
  }

  try {
    const cosigners = await buildCosigners(context.live!, parsed.total, scheme, `${scenarioId}-${Date.now()}`);
    const commitment = await guardianCommitment(cosigners[0], scheme);

    const multisig = await cosigners[0].multisigClient.create(
      {
        threshold: parsed.threshold,
        signerCommitments: cosigners.map((cosigner) => cosigner.signer.commitment),
        guardianCommitment: commitment,
        signatureScheme: scheme,
      },
      cosigners[0].signer,
    );

    sessions.set(scenarioId, {
      cosigners,
      threshold: parsed.threshold,
      scheme,
      multisig,
      accountId: multisig.accountId,
      balanceSeen: false,
    });
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `creating the multisig account failed: ${String(error)}`,
    };
  }
}

export async function registerAccount(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  try {
    await session.multisig.registerOnGuardian();
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `registering ${session.accountId} with GUARDIAN failed: ${String(error)}`,
    };
  }
}

export async function verifyRegistration(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  try {
    const state = await session.multisig.syncState();
    if (!state) {
      return { kind: 'failed', classification: 'product', reason: 'GUARDIAN returned no state for the account' };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `GUARDIAN would not serve state for ${session.accountId}: ${String(error)}`,
    };
  }
}

interface FundingRecord {
  readonly amount: number;
  readonly faucet: string;
}

async function fundOnce(context: ActionContext, session: LiveSession): Promise<FundingRecord> {
  const funded = await fundAccount({
    network: context.live!.network,
    recipient: session.accountId!,
    amount: ACCOUNT_FUNDING,
  });
  session.faucetId = funded.faucet;
  session.treasuryId = funded.treasury;
  // Accumulated, not assigned, and for the reason the Rust driver records at
  // its own funding sites: a scenario that runs both `asset-transfer` and
  // `proposal-create` sends two notes, the consume proposal takes whichever
  // have committed, and recording only the last one made the balance assertion
  // fail whenever both landed.
  session.transferred = (session.transferred ?? 0n) + BigInt(funded.amount);
  return { amount: funded.amount, faucet: funded.faucet };
}

/**
 * Waits for a transferred note to become consumable.
 *
 * A submitted transfer is not yet a visible note: it has to be committed in a
 * block first. Polled against a deadline so a slow network is distinguishable
 * from one that lost it. Clients are built with autoSync off, so nothing syncs
 * the chain unless asked, and GUARDIAN's view knows nothing about an inbound
 * note either.
 */
async function waitForConsumableNotes(session: LiveSession): Promise<string[] | null> {
  const deadline = Date.now() + NOTE_ARRIVAL_DEADLINE_MS;
  let wait = POLL_START_MS;
  while (Date.now() < deadline) {
    try {
      await session.cosigners[0].midenClient.sync();
      await session.multisig!.syncState();
      const notes = await session.multisig!.getConsumableNotes();
      if (notes.length > 0) return notes.map((note) => note.id);
    } catch {
      // A sync that loses a race with block production is retried, not fatal.
    }
    wait = await backoff(wait);
  }
  return null;
}

/**
 * Collects signatures until `target` is reached.
 *
 * Every cosigner is offered the proposal, including the proposer, rather than
 * assuming who has already signed. The two SDKs differ here: creating a
 * proposal in Rust carries the proposer's signature, and in TypeScript it does
 * not, so a loop that skipped the proposer would come up one short.
 */
async function collectSignatures(
  session: LiveSession,
  target: number,
): Promise<{ collected: number; error?: string }> {
  let collected = 0;
  for (let index = 0; index < session.cosigners.length; index += 1) {
    if (collected >= target) break;
    const cosigner = session.cosigners[index];
    try {
      const loaded =
        index === 0
          ? session.multisig!
          : await cosigner.multisigClient.load(session.accountId!, cosigner.signer);
      await cosigner.midenClient.sync();
      await loaded.syncState();
      await loaded.syncProposals();
      const signed = await loaded.signProposal(session.proposalId!);
      collected = Math.min(signed.signatures?.length ?? collected + 1, target);
    } catch (error) {
      const message = String(error);
      if (message.includes('already signed')) continue;
      return { collected, error: `cosigner ${index} could not sign: ${message}` };
    }
  }
  return { collected };
}

function normalizeHex(value: string): string {
  return value.replace(/^0x/, '').toLowerCase();
}

/** What the chain and GUARDIAN say about an executed proposal. */
type Completion =
  | { kind: 'confirmed' }
  | { kind: 'discarded'; reason: string }
  | { kind: 'pending'; reason: string };

/**
 * Waits for an executed proposal to be provably complete.
 *
 * Completion is not "the proposal disappeared": canonicalization removes the
 * proposal whether it applied the delta or gave up on it. So this asks the SDK
 * the same questions a consumer would, whether local state matches the chain and
 * whether GUARDIAN's canonical history carries that state, instead of
 * re-deriving an answer from the pending list.
 */
/**
 * The nonce a proposal will land at, read while it is still listed.
 *
 * Completion has to be bound to the proposal it was asked about, and once the
 * proposal leaves the pending set there is nothing left to read the nonce from,
 * so callers capture it before executing.
 */
async function proposalNonce(
  session: LiveSession,
  proposalId: string,
): Promise<{ nonce: number } | { reason: string }> {
  try {
    const proposals = await session.multisig!.syncProposals();
    const mine = proposals.find((proposal) => proposal.id === proposalId);
    return mine ? { nonce: Number(mine.nonce) } : { reason: `proposal ${proposalId} is not listed` };
  } catch (error) {
    return { reason: `listing proposals failed: ${String(error)}` };
  }
}

/**
 * What ties an executed proposal's completion to that proposal: the nonce its
 * canonical delta must land at, or, for a migration, chain agreement alone.
 */
type Binding = { kind: 'nonce'; nonce: number } | { kind: 'migration' };

/**
 * Reads the binding before executing, and refuses to execute without one.
 *
 * Completion cannot be confirmed without the nonce, so executing anyway spent
 * the transaction, waited out the whole canonicalization deadline and then
 * reported the product, for what was never more than a listing the harness
 * could not read. An offline document carries the nonce it was signed at, so
 * an offline proposal GUARDIAN never listed is still bound.
 */
async function bindExecution(
  session: LiveSession,
  proposalId: string,
): Promise<Binding | { failure: ActionOutcome }> {
  if (session.migrating) return { kind: 'migration' };
  if (session.exportedProposal) {
    const document = JSON.parse(session.exportedProposal) as { commitment?: unknown; nonce?: unknown };
    if (document.commitment === proposalId && typeof document.nonce === 'number') {
      return { kind: 'nonce', nonce: document.nonce };
    }
  }
  const read = await proposalNonce(session, proposalId);
  if ('nonce' in read) return { kind: 'nonce', nonce: read.nonce };
  return {
    failure: {
      kind: 'failed',
      classification: 'setup',
      reason: `the proposal nonce could not be read before executing, so completion could not be bound to it; nothing was executed: ${read.reason}`,
    },
  };
}

async function waitForExecution(
  session: LiveSession,
  proposalId: string,
  binding: Binding,
): Promise<Completion> {
  const deadline = Date.now() + CANONICALIZATION_DEADLINE_MS;
  let wait = POLL_START_MS;
  let last = 'never answered';

  while (Date.now() < deadline) {
    try {
      await session.cosigners[0].midenClient.sync();
    } catch {
      // A sync that loses a race with block production is retried, not fatal.
    }

    let stillPending = true;
    try {
      const proposals = await session.multisig!.syncProposals();
      // Presence is not enough: the listing is the client's own cache, and an
      // executed proposal stays in it marked `finalized` until GUARDIAN stops
      // reporting it. A finalized entry is done, not pending.
      const mine = proposals.find((proposal) => proposal.id === proposalId);
      stillPending = mine !== undefined && mine.status !== 'finalized';
      if (stillPending) {
        last = `the proposal is still listed with status ${mine?.status ?? 'unknown'}`;
      }
    } catch (error) {
      last = String(error);
    }

    if (!stillPending) {
      try {
        const verified = await session.multisig!.verifyStateCommitment();
        const commitment = normalizeHex(verified.onChainCommitment);

        // A migration repoints the client at the GUARDIAN it just moved to, and
        // that GUARDIAN has no history for an account it has only just been
        // handed. The canonical delta stays with the one being left behind, so
        // demanding it here would wait for something that cannot arrive.
        // Chain agreement plus the new GUARDIAN serving the account is what
        // completion means for this proposal type.
        if (binding.kind === 'migration') {
          const state = await session.multisig!.syncState();
          if (state) return { kind: 'confirmed' };
          last = 'the new GUARDIAN does not serve the migrated account';
        } else {
          // Bound to the proposal, not merely to the account being
          // self-consistent. Matching on the commitment alone asks "is this
          // account in a state some canonical delta explains", which an account
          // whose delta was discarded satisfies just as well: it never moved, so
          // it still agrees with chain and the *previous* delta still carries
          // that commitment. The nonce ties the answer to the delta under test.
          const { nonce } = binding;
          const history = await session.multisig!.deltaHistory({ limit: 20 });
          const canonical = history.entries.some(
            (entry) =>
              entry.newCommitment &&
              normalizeHex(entry.newCommitment) === commitment &&
              Number(entry.nonce) === nonce,
          );
          if (canonical) return { kind: 'confirmed' };
          last = `no canonical delta at nonce ${nonce} carries commitment ${commitment}`;
        }
      } catch (error) {
        last = String(error);
      }
    }

    wait = await backoff(wait);
  }

  try {
    const proposals = await session.multisig!.syncProposals();
    if (!proposals.some((proposal) => proposal.id === proposalId)) {
      return { kind: 'discarded', reason: last };
    }
  } catch {
    // Fall through to pending: an unreadable list is not proof of discard.
  }
  return {
    kind: 'pending',
    reason: `still pending after ${CANONICALIZATION_DEADLINE_MS / 1000}s; ${last}`,
  };
}

/**
 * Funds the account and proposes consuming that funding note.
 *
 * Forced ordering, not a choice: on a fee-charging chain the account cannot
 * execute anything until it holds the fee asset, and the only way in is to
 * consume an inbound note, which for a multisig is a proposal.
 */
export async function createProposal(context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  const missing = requireLive(context);
  if (missing) return missing;

  try {
    const funded = await fundOnce(context, session);
    if (funded.amount === 0) {
      return { kind: 'skipped', reason: 'this chain charges nothing, so there is no funding note to consume' };
    }
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'setup',
      reason: `cannot fund ${session.accountId}: ${String(error)}`,
    };
  }

  const noteIds = await waitForConsumableNotes(session);
  if (!noteIds) {
    return {
      kind: 'environment_blocked',
      reason: `the funding note did not reach the account within ${NOTE_ARRIVAL_DEADLINE_MS / 1000}s`,
    };
  }

  try {
    const proposal = await session.multisig.createConsumeNotesProposal(noteIds);
    session.proposalId = proposal.id;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `creating the consume-notes proposal failed: ${String(error)}`,
    };
  }
}

export async function signProposal(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.proposalId || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }

  const { collected, error } = await collectSignatures(session, session.threshold);
  if (error) return { kind: 'failed', classification: 'product', reason: error };
  if (collected < session.threshold) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `collected ${collected} signature(s) but the threshold is ${session.threshold}`,
    };
  }
  return { kind: 'passed' };
}

async function chainNonce(session: LiveSession): Promise<bigint | null> {
  try {
    await session.cosigners[0].midenClient.sync();
    const account = await session.multisig!.getStoreAccount();
    return account.nonce().asInt();
  } catch {
    return null;
  }
}

export async function executeProposal(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }

  const nonceBefore = await chainNonce(session);
  // Read before executing: once the proposal leaves the pending set there is
  // nothing left to read it from, and completion has to be bound to it.
  const proposalLandsAt = await bindExecution(session, session.proposalId);
  if ('failure' in proposalLandsAt) return proposalLandsAt.failure;

  try {
    // Signatures were added through the other cosigners' clients, so the
    // executing client's proposal cache does not have them yet. It is rebuilt
    // from GUARDIAN rather than assumed.
    await session.cosigners[0].midenClient.sync();
    await session.multisig.syncProposals();
    await session.multisig.executeProposal(session.proposalId);
    await session.multisig.syncState();
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `executing the proposal failed: ${String(error)}`,
    };
  }

  const completion = await waitForExecution(session, session.proposalId, proposalLandsAt);
  if (completion.kind === 'confirmed') return { kind: 'passed' };
  if (completion.kind === 'discarded') {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the proposal left the pending set without becoming canonical: ${completion.reason}`,
    };
  }

  // Still pending means either the chain never took the transaction or GUARDIAN
  // has not caught up, and only the account's own nonce separates them.
  // Corroboration, not the definition of done.
  const nonceAfter = await chainNonce(session);
  if (nonceBefore !== null && nonceAfter !== null && nonceAfter === nonceBefore) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the proposal was executed but the account nonce never moved past ${nonceBefore}, so nothing reached the chain`,
    };
  }

  return { kind: 'environment_blocked', reason: `the executed proposal was ${completion.reason}` };
}

/**
 * A proposal one signature short of threshold must not execute, and must
 * survive the attempt so the remaining cosigner can still sign it.
 */
export async function rejectBelowThreshold(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }
  if (session.threshold < 2) {
    return { kind: 'skipped', reason: 'a 1-of-n account has no below-threshold state' };
  }

  const target = session.threshold - 1;
  const { collected, error } = await collectSignatures(session, target);
  if (error) return { kind: 'failed', classification: 'product', reason: error };
  if (collected !== target) {
    return {
      kind: 'failed',
      classification: 'setup',
      reason: `wanted exactly ${target} signature(s) to test the boundary, got ${collected}`,
    };
  }

  const nonceBefore = await chainNonce(session);
  try {
    await session.multisig.syncProposals();
    await session.multisig.executeProposal(session.proposalId);
  } catch (caught) {
    const message = String(caught);
    if (!/pending signatures|not ready/i.test(message)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the proposal was refused, but not for being below threshold: ${message}`,
      };
    }
    // The refusal here is the client's own pre-flight check, and GUARDIAN
    // acknowledges a delta without counting cosigner signatures, so confirm
    // nothing reached the chain rather than assuming the refusal stopped it.
    const nonceAfter = await chainNonce(session);
    if (nonceBefore !== null && nonceAfter !== null && nonceAfter !== nonceBefore) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: 'the account nonce advanced after a below-threshold execution was refused',
      };
    }
    const remaining = await session.multisig.syncProposals();
    if (!remaining.some((proposal) => proposal.id === session.proposalId)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: 'the refused proposal was discarded instead of staying pending',
      };
    }
    return { kind: 'passed' };
  }

  return {
    kind: 'failed',
    classification: 'product',
    reason: `a proposal with ${collected} of ${session.threshold} signatures executed`,
  };
}

/** Signing twice with one key must not count twice toward the threshold. */
export async function rejectDuplicateSignature(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }

  const countSignatures = async (): Promise<number> => {
    const proposals = await session.multisig!.syncProposals();
    return proposals.find((proposal) => proposal.id === session.proposalId)?.signatures.length ?? 0;
  };

  const before = await countSignatures();
  try {
    await session.multisig.signProposal(session.proposalId);
  } catch (caught) {
    const message = String(caught);
    if (!/already signed/i.test(message)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the duplicate was refused, but not as a duplicate: ${message}`,
      };
    }
    const after = await countSignatures();
    if (after !== before) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the refused duplicate still changed the signature count from ${before} to ${after}`,
      };
    }
    return { kind: 'passed' };
  }

  const after = await countSignatures();
  return {
    kind: 'failed',
    classification: 'product',
    reason: `a duplicate signature was accepted; the count went from ${before} to ${after}`,
  };
}

/**
 * A cosigner holding only its own key rebuilds the account from GUARDIAN.
 *
 * The recovering cosigner is the one that has taken no part in the scenario, and
 * each cosigner has its own store, so nothing local can be standing in for what
 * GUARDIAN serves.
 */
export async function recoverByCosigner(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }

  const cosigner = session.cosigners[session.cosigners.length - 1];
  try {
    // Discovery first, by key alone. Loading a known account id is a weaker
    // operation: it proves GUARDIAN serves state for an account you can already
    // name, not that a cosigner holding only its key can find the account at
    // all. The Rust driver calls `recover_by_key` here, so calling `load` was
    // the two drivers proving different things under one scenario name, which
    // is the drift this suite exists to catch.
    const discovered = await cosigner.multisigClient.recoverByKey(cosigner.signer);
    if (!discovered.some((entry) => entry.accountId === session.accountId)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason:
          `GUARDIAN did not offer ${session.accountId} to a cosigner holding one of its keys; ` +
          `it offered [${discovered.map((entry) => entry.accountId).join(', ')}]`,
      };
    }

    const recovered = await cosigner.multisigClient.load(session.accountId, cosigner.signer);

    const [expectedSigners, actualSigners] = await Promise.all([
      session.multisig.getSignerPublicKeyCommitments(),
      recovered.getSignerPublicKeyCommitments(),
    ]);
    if (JSON.stringify([...expectedSigners].sort()) !== JSON.stringify([...actualSigners].sort())) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the recovered signer set ${actualSigners.join(',')} differs from ${expectedSigners.join(',')}`,
      };
    }

    const [expectedGuardian, actualGuardian] = await Promise.all([
      session.multisig.getGuardianPublicKeyCommitment(),
      recovered.getGuardianPublicKeyCommitment(),
    ]);
    if (expectedGuardian !== actualGuardian) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the recovered GUARDIAN commitment ${actualGuardian} differs from ${expectedGuardian}`,
      };
    }

    if (recovered.accountId !== session.accountId) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `recovery produced account ${recovered.accountId}, not ${session.accountId}`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the cosigner could not recover ${session.accountId}: ${String(error)}`,
    };
  }
}

/**
 * Asserts the account's holdings, first as an empty baseline and then against
 * what the transfer delivered.
 *
 * The second reading is bounded rather than exact: executing the consuming
 * transaction pays a fee out of the same asset, so the account keeps less than
 * it received.
 */
export async function assertBalance(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }

  try {
    const account = await session.multisig.getStoreAccount();
    const vault = account.vault();

    if (!session.balanceSeen) {
      const held = vault.fungibleAssets();
      if (held.length > 0) {
        return {
          kind: 'failed',
          classification: 'product',
          reason: `a freshly created account already holds ${held.length} asset(s)`,
        };
      }
      session.balanceSeen = true;
      return { kind: 'passed' };
    }

    if (!session.faucetId || session.transferred === undefined) {
      return { kind: 'failed', classification: 'setup', reason: 'nothing was transferred to this account' };
    }

    const balance = vault.getBalance(AccountId.fromHex(session.faucetId));
    if (balance <= 0n) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the account holds nothing of faucet ${session.faucetId} after consuming the note`,
      };
    }
    if (balance > session.transferred) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the account holds ${balance} but only ${session.transferred} was transferred`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading the account balance failed: ${String(error)}`,
    };
  }
}

/** Sends assets to the multisig account from the treasury. */
export async function transferAsset(
  context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  const missing = requireLive(context);
  if (missing) return missing;

  try {
    const funded = await fundOnce(context, session);
    if (funded.amount === 0) {
      return { kind: 'skipped', reason: 'this chain charges nothing, so there is nothing to transfer' };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'setup',
      reason: `cannot transfer to ${session.accountId}: ${String(error)}`,
    };
  }
}

/** Consumes the transferred note through a full proposal lifecycle. */
export async function consumeNote(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }

  const noteIds = await waitForConsumableNotes(session);
  if (!noteIds) {
    return {
      kind: 'environment_blocked',
      reason: `the transferred note did not reach the account within ${NOTE_ARRIVAL_DEADLINE_MS / 1000}s`,
    };
  }

  try {
    const proposal = await session.multisig.createConsumeNotesProposal(noteIds);
    session.proposalId = proposal.id;
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `creating the consume-notes proposal failed: ${String(error)}`,
    };
  }

  const { collected, error } = await collectSignatures(session, session.threshold);
  if (error) return { kind: 'failed', classification: 'product', reason: error };
  if (collected < session.threshold) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `collected ${collected} signature(s) but the threshold is ${session.threshold}`,
    };
  }

  const consumeLandsAt = await bindExecution(session, session.proposalId);
  if ('failure' in consumeLandsAt) return consumeLandsAt.failure;

  try {
    await session.cosigners[0].midenClient.sync();
    await session.multisig.syncProposals();
    await session.multisig.executeProposal(session.proposalId);
    await session.multisig.syncState();
  } catch (caught) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `consuming the note failed: ${String(caught)}`,
    };
  }

  const completion = await waitForExecution(session, session.proposalId, consumeLandsAt);
  if (completion.kind === 'discarded') {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the consuming proposal left the pending set without becoming canonical: ${completion.reason}`,
    };
  }
  if (completion.kind === 'pending') {
    return { kind: 'environment_blocked', reason: `the consuming proposal was ${completion.reason}` };
  }

  await session.cosigners[0].midenClient.sync();
  await session.multisig.syncState();
  return { kind: 'passed' };
}

/** Serializes the pending proposal for transport over a side channel. */
export async function exportProposal(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }

  try {
    session.exportedProposal = session.multisig.exportProposalToJson(session.proposalId);
    const parsed = JSON.parse(session.exportedProposal) as {
      commitment?: string;
      txSummaryBase64?: string;
    };
    if (parsed.commitment !== session.proposalId || !parsed.txSummaryBase64) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: 'the exported proposal does not carry the proposal it was asked for',
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `exporting the proposal failed: ${String(error)}`,
    };
  }
}

/**
 * Signs the exported proposal on each cosigner's own client until the threshold
 * is met, passing the document along rather than any shared state.
 *
 * Nothing reaches GUARDIAN here: the point of the offline path is that the
 * signatures travel inside the document.
 */
export async function signProposalExternally(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId || !session.exportedProposal) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been exported in this scenario' };
  }

  let document = session.exportedProposal;
  let signatures = (JSON.parse(document) as { signatures?: unknown[] }).signatures?.length ?? 0;

  for (let index = 0; index < session.cosigners.length; index += 1) {
    if (signatures >= session.threshold) break;
    const cosigner = session.cosigners[index];
    try {
      const loaded =
        index === 0
          ? session.multisig
          : await cosigner.multisigClient.load(session.accountId, cosigner.signer);
      const imported = await loaded.importProposal(document);
      document = await loaded.signProposalOffline(imported.id);
      signatures = (JSON.parse(document) as { signatures?: unknown[] }).signatures?.length ?? signatures;
    } catch (error) {
      const message = String(error);
      if (/already signed/i.test(message)) continue;
      return {
        kind: 'failed',
        classification: 'product',
        reason: `cosigner ${index} could not sign offline: ${message}`,
      };
    }
  }

  if (signatures < session.threshold) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the offline document carries ${signatures} signature(s) but the threshold is ${session.threshold}`,
    };
  }

  session.exportedProposal = document;
  return { kind: 'passed' };
}

/** Brings the externally signed document back into the executing client. */
export async function importProposal(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.exportedProposal) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been signed offline in this scenario' };
  }

  try {
    const imported = await session.multisig.importProposal(session.exportedProposal);
    if (imported.signatures.length < session.threshold) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the imported proposal kept ${imported.signatures.length} of ${session.threshold} signatures`,
      };
    }
    session.proposalId = imported.id;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `importing the signed proposal failed: ${String(error)}`,
    };
  }
}

/**
 * Builds a GUARDIAN migration proposal without contacting GUARDIAN.
 *
 * Migration needs a second deployment to migrate to: the account's guardian
 * slot must actually change, and re-pointing it at the GUARDIAN it already uses
 * produces no state change for the transaction to commit. The endpoint is
 * supplied by the stack rather than assumed, so a run without one reports that
 * it could not test migration instead of testing something else.
 */
/**
 * Rotates GUARDIAN through the pending set instead of around it.
 *
 * The offline path is the air-gapped one: nothing reaches GUARDIAN until the
 * signed document is imported. This is the half a deployment actually uses when
 * GUARDIAN is reachable, and rotation is a first-class custody operation, so
 * qualifying only the air-gapped path left the common one untested.
 *
 * The distinction is asserted rather than assumed, and the assertion has to go
 * through `syncProposals`: `listProposals` returns this client's own cache,
 * where a proposal created offline would sit just as happily. Only
 * `syncProposals` asks GUARDIAN. That is a genuine difference from the Rust
 * SDK, whose `list_proposals` fetches.
 */
export async function switchGuardianOnline(
  context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  const missing = requireLive(context);
  if (missing) return missing;

  const target = context.live!.migrationEndpoint;
  if (!target) {
    return {
      kind: 'environment_blocked',
      reason:
        'rotation needs a second GUARDIAN to rotate to; set QUAL_GUARDIAN_MIGRATION_ENDPOINT to one',
    };
  }

  try {
    const destination = new GuardianHttpClient(target);
    const pubkey = await destination.getPubkey(session.scheme);
    const commitment = typeof pubkey === 'string' ? pubkey : pubkey.commitment;

    const current = await session.multisig.getGuardianPublicKeyCommitment();
    if (commitment === current) {
      return {
        kind: 'environment_blocked',
        reason: `the rotation target at ${target} has the same identity as the current GUARDIAN`,
      };
    }

    const proposal = await session.multisig.createSwitchGuardianProposal(target, commitment);

    const pending = await session.multisig.syncProposals();
    if (!pending.some((entry) => entry.id === proposal.id)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason:
          `the rotation proposal ${proposal.id} is not in GUARDIAN's pending set, so it was ` +
          'not coordinated online',
      };
    }

    session.proposalId = proposal.id;
    // Completion is judged differently for a rotation: the GUARDIAN the client
    // moves to has no history for an account it was just handed.
    session.migrating = true;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing a rotation to ${target} through GUARDIAN failed: ${String(error)}`,
    };
  }
}

/**
 * Confirms the rotation moved the account, rather than only executing.
 *
 * The offline scenario asserts the shape of the document it produced, which
 * says nothing about the account. A rotation that executes without changing the
 * bound identity is the failure worth catching, because every other signal
 * looks like success.
 */
export async function assertGuardianSwitched(
  context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  const missing = requireLive(context);
  if (missing) return missing;

  const target = context.live!.migrationEndpoint;
  if (!target) {
    return { kind: 'failed', classification: 'setup', reason: 'no rotation target is configured' };
  }

  try {
    const destination = new GuardianHttpClient(target);
    const pubkey = await destination.getPubkey(session.scheme);
    const expected = typeof pubkey === 'string' ? pubkey : pubkey.commitment;

    const bound = await session.multisig.getGuardianPublicKeyCommitment();
    if (bound !== expected) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the rotation executed but the account still binds ${bound}, not ${expected} at ${target}`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the account's GUARDIAN binding could not be read after the rotation: ${String(error)}`,
    };
  }
}

/**
 * A paused account is refused on a live network, and works again once unpaused.
 *
 * `det-account-paused` proves GUARDIAN's gate without a chain. This is the half
 * that matters to custody: execution needs GUARDIAN's acknowledgement, so a
 * paused account cannot execute and nothing lands. The proposal is created
 * before the pause deliberately, so the refusal falls on execution rather than
 * on creation, which is the gate standing between a client and the chain.
 *
 * Causation is proved structurally rather than by reading the message: the next
 * action executes the same proposal with the same client and must succeed once
 * unpaused. GUARDIAN answers `GUARDIAN_ACCOUNT_PAUSED`, but by the time a
 * refusal reaches this driver only the status and the human-readable message
 * survive, so the wording check only rules out a refusal that obviously has
 * nothing to do with pausing.
 *
 * Unpauses whatever the attempt concluded; an account left paused fails the
 * rest of the scenario for an unrelated reason.
 */
export async function assertPausedRefusesExecution(
  context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  if (!session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }

  const paused = await setAccountPaused(context, session.accountId, true);
  if (paused) return paused;

  let attempt: ActionOutcome;
  try {
    await session.multisig.executeProposal(session.proposalId);
    attempt = {
      kind: 'failed',
      classification: 'product',
      reason:
        'a paused account executed a proposal; the pause did not stand between the client and the chain',
    };
  } catch (error) {
    const rendered = String(error);
    attempt = rendered.includes('account is paused')
      ? { kind: 'passed' }
      : {
          kind: 'failed',
          classification: 'product',
          reason: `the paused account was refused, but not as paused: ${rendered}`,
        };
  }

  const unpaused = await setAccountPaused(context, session.accountId, false);
  if (unpaused) return attempt.kind === 'passed' ? unpaused : attempt;
  return attempt;
}

export async function createProposalOffline(
  context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  const missing = requireLive(context);
  if (missing) return missing;

  const target = context.live!.migrationEndpoint;
  if (!target) {
    return {
      kind: 'environment_blocked',
      reason:
        'migration needs a second GUARDIAN deployment; set QUAL_GUARDIAN_MIGRATION_ENDPOINT to one',
    };
  }

  try {
    const destination = new GuardianHttpClient(target);
    const pubkey = await destination.getPubkey(session.scheme);
    const commitment = typeof pubkey === 'string' ? pubkey : pubkey.commitment;

    const current = await session.multisig.getGuardianPublicKeyCommitment();
    if (commitment === current) {
      return {
        kind: 'environment_blocked',
        reason: `the migration target at ${target} has the same identity as the current GUARDIAN`,
      };
    }

    const exported = await session.multisig.createSwitchGuardianProposalOffline(target, commitment);
    session.exportedProposal = JSON.stringify(exported);
    session.proposalId = exported.commitment;
    session.migrating = true;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `building the offline migration proposal failed: ${String(error)}`,
    };
  }
}

/** Checks the offline proposal really is a GUARDIAN migration. */
export async function assertGuardianMigration(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.exportedProposal) {
    return { kind: 'failed', classification: 'setup', reason: 'no offline proposal has been created in this scenario' };
  }

  const exported = JSON.parse(session.exportedProposal) as {
    metadata?: { proposalType?: string; newGuardianEndpoint?: string; newGuardianPubkey?: string };
  };
  const metadata = exported.metadata;
  if (metadata?.proposalType !== 'switch_guardian') {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the offline proposal is a ${metadata?.proposalType ?? 'unknown'} proposal, not a migration`,
    };
  }
  if (!metadata.newGuardianEndpoint || !metadata.newGuardianPubkey) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: 'the migration proposal does not name the GUARDIAN it migrates to',
    };
  }
  return { kind: 'passed' };
}

/**
 * Hands one cosigner's key to the Rust driver and has it sign the proposal.
 *
 * The signature has to come from the other SDK's own process for the scenario
 * to mean anything, so this shells out rather than signing here. The key
 * travels in a file, not an argument, so it stays out of the process list.
 */
export async function handoffToRust(
  context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.accountId || !session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }
  const missing = requireLive(context);
  if (missing) return missing;

  const cosigner = session.cosigners[1];
  if (!cosigner) {
    return { kind: 'failed', classification: 'setup', reason: 'this shape has no second cosigner to hand off to' };
  }

  // Creating a proposal in TypeScript does not sign it, unlike Rust, so the
  // proposing side contributes its own signature before handing over. Without
  // it a 2-of-3 reaches only the one signature the other SDK adds.
  const local = await collectSignatures(session, 1);
  if (local.error) {
    return { kind: 'failed', classification: 'product', reason: local.error };
  }

  const keyFile = join(
    mkdtempSync(join(tmpdir(), 'qual-handoff-')),
    'cosigner.key',
  );
  try {
    writeFileSync(keyFile, bytesToHex(cosigner.secretKey.serialize()), { mode: 0o600 });
    await cosignWithRust({
      network: context.live!.network,
      // The Rust SDK speaks gRPC to GUARDIAN; the HTTP endpoint this driver
      // uses would not answer it.
      guardianEndpoint: context.grpcEndpoint,
      accountId: session.accountId,
      proposalId: session.proposalId,
      keyFile,
    });
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the Rust driver could not sign ${session.proposalId}: ${String(error)}`,
    };
  } finally {
    rmSync(dirname(keyFile), { recursive: true, force: true });
  }

  try {
    await session.multisig!.syncProposals();
    const signed = session.multisig!.listProposals().find(
      (proposal) => proposal.id === session.proposalId,
    );
    const commitment = cosigner.signer.commitment.toLowerCase();
    const present = signed?.signatures.some(
      (entry) => entry.signerId.toLowerCase() === commitment,
    );
    if (!present) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: "the Rust driver reported success but its signature is not on the proposal",
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading back the handed-off signature failed: ${String(error)}`,
    };
  }
}

/**
 * How long GUARDIAN may take to finish applying an executed change before the
 * suite treats the disagreement as real.
 */
const SETTLE_DEADLINE_MS = 180_000;

/**
 * Proposes, waiting out the window where GUARDIAN still reports the previous
 * change as pending. Creating a proposal is a GUARDIAN call, not a chain
 * submission, so retrying it risks nothing.
 */
async function proposeWhenSettled<T>(propose: () => Promise<T>): Promise<T> {
  const deadline = Date.now() + SETTLE_DEADLINE_MS;
  let wait = POLL_START_MS;
  for (;;) {
    try {
      return await propose();
    } catch (error) {
      const message = String(error);
      if (!message.includes('already a pending change') || Date.now() >= deadline) throw error;
      wait = await backoff(wait);
    }
  }
}

function normalizeCommitments(commitments: readonly string[]): string[] {
  return [...commitments].map((entry) => entry.toLowerCase()).sort();
}

async function currentSignerSet(session: LiveSession): Promise<string[]> {
  return normalizeCommitments(await session.multisig!.getSignerPublicKeyCommitments());
}

/**
 * Proposes admitting a signer that does not yet hold the account.
 *
 * The incoming signer gets its own client and is kept out of the cosigner list
 * until the change executes: a key that could sign its own admission would make
 * the threshold meaningless.
 */
export async function addSigner(context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  const missing = requireLive(context);
  if (missing) return missing;

  try {
    const [incoming] = await buildCosigners(
      context.live!,
      1,
      session.scheme,
      `${scenarioId}-incoming-${Date.now()}`,
    );
    const before = await currentSignerSet(session);
    const commitment = incoming.signer.commitment.toLowerCase();
    if (before.includes(commitment)) {
      return { kind: 'failed', classification: 'setup', reason: 'the incoming signer already holds the account' };
    }

    const proposal = await proposeWhenSettled(() =>
      session.multisig!.createAddSignerProposal(incoming.signer.commitment),
    );
    session.proposalId = proposal.id;
    session.incoming = incoming;
    session.expectedSigners = normalizeCommitments([...before, commitment]);
    session.expectedThreshold = session.threshold;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing the added signer failed: ${String(error)}`,
    };
  }
}

/** Proposes removing the cosigner that has taken no part in the scenario. */
export async function removeSigner(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }

  try {
    const before = await currentSignerSet(session);
    const departing = session.cosigners[session.cosigners.length - 1].signer.commitment.toLowerCase();
    if (!before.includes(departing)) {
      return { kind: 'failed', classification: 'setup', reason: `${departing} is not in the signer set` };
    }
    if (before.length - 1 < session.threshold) {
      return { kind: 'skipped', reason: 'removing a signer would drop the set below its own threshold' };
    }

    const proposal = await proposeWhenSettled(() =>
      session.multisig!.createRemoveSignerProposal(departing),
    );
    session.proposalId = proposal.id;
    session.departed = session.cosigners[session.cosigners.length - 1];
    session.expectedSigners = before.filter((entry) => entry !== departing);
    session.expectedThreshold = session.threshold;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing the removed signer failed: ${String(error)}`,
    };
  }
}

/** Proposes raising the threshold to require every current signer. */
export async function changeThreshold(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }

  try {
    const before = await currentSignerSet(session);
    const target = before.length;
    if (target === session.threshold) {
      return { kind: 'skipped', reason: 'the threshold already requires every signer' };
    }

    const proposal = await proposeWhenSettled(() =>
      session.multisig!.createChangeThresholdProposal(target),
    );
    session.proposalId = proposal.id;
    session.expectedSigners = before;
    session.expectedThreshold = target;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing the threshold change failed: ${String(error)}`,
    };
  }
}

/**
 * Asserts the executed membership change landed, on chain and in GUARDIAN's
 * view. Both are checked because a change visible in only one of them is the
 * divergence this suite exists to catch.
 */
export async function assertSignerSet(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.expectedSigners || session.expectedThreshold === undefined) {
    return { kind: 'failed', classification: 'setup', reason: 'no membership change was proposed in this scenario' };
  }

  try {
    await session.cosigners[0].midenClient.sync();
    await session.multisig.syncState();

    const onChain = await currentSignerSet(session);
    if (JSON.stringify(onChain) !== JSON.stringify(session.expectedSigners)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the on-chain signer set is ${onChain.join(',')} but ${session.expectedSigners.join(',')} was expected`,
      };
    }

    const account = await session.multisig.getStoreAccount();
    const detected = AccountInspector.fromAccount(account);
    if (detected.threshold !== session.expectedThreshold) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the on-chain threshold is ${detected.threshold} but ${session.expectedThreshold} was expected`,
      };
    }

    // GUARDIAN's own view, reached through a fresh load rather than the client
    // that just executed, so a stale local cache cannot answer for it. Polled,
    // not read once: a proposal leaving the pending set does not mean GUARDIAN
    // has finished applying it, and its authorization list can still be the
    // pre-change one, which refuses a newly admitted signer outright.
    // A client that still holds the account and did not execute the change. The
    // obvious candidate, the last cosigner, is the one a removal just took
    // away, and a removed key cannot authenticate by design.
    const remaining = session.cosigners
      .slice(1)
      .filter((cosigner) => session.expectedSigners!.includes(cosigner.signer.commitment.toLowerCase()));
    const reader = session.incoming ?? remaining[remaining.length - 1] ?? session.cosigners[0];
    const deadline = Date.now() + SETTLE_DEADLINE_MS;
    let wait = POLL_START_MS;
    let last = 'never answered';
    while (Date.now() < deadline) {
      try {
        const reloaded = await reader.multisigClient.load(session.accountId!, reader.signer);
        // Read the account `load` fetched from GUARDIAN, not the client's stored
        // one. `getSignerPublicKeyCommitments()` reads the store, so asserting
        // on it measures the client while reporting the result as GUARDIAN's.
        // That misattribution hid a correct GUARDIAN behind a client-side
        // staleness bug for five reproductions.
        const served = normalizeCommitments(
          AccountInspector.getSignerPublicKeyCommitments(reloaded.account),
        );
        if (JSON.stringify(served) === JSON.stringify(session.expectedSigners)) {
          return { kind: 'passed' };
        }
        const stored = normalizeCommitments(await reloaded.getSignerPublicKeyCommitments());
        last =
          `serves ${served.join(',')}` +
          (JSON.stringify(stored) === JSON.stringify(served)
            ? ''
            : ` (the reader's store says ${stored.join(',')}, which is a client-side` +
              ` staleness bug and not what is asserted here)`);
      } catch (error) {
        last = String(error);
      }
      wait = await backoff(wait);
    }
    return {
      kind: 'failed',
      classification: 'product',
      reason: `GUARDIAN ${last} but ${session.expectedSigners.join(',')} was expected after ${SETTLE_DEADLINE_MS / 1000}s`,
    };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading back the membership change failed: ${String(error)}`,
    };
  }
}

/** What the account sends back to the treasury, well under what it holds. */
const P2ID_AMOUNT = 1_000n;

/** The procedure whose threshold override the suite exercises. */
const OVERRIDE_PROCEDURE = 'send_asset';

async function heldBalance(session: LiveSession): Promise<bigint> {
  const account = await session.multisig!.getStoreAccount();
  return account.vault().getBalance(AccountId.fromHex(session.faucetId!));
}

/**
 * Proposes sending assets out of the multisig, back to the treasury that funded
 * it. A real counterparty rather than the account itself, so the note has to
 * leave the vault to somewhere that can actually claim it.
 */
export async function sendAsset(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  if (!session.faucetId || !session.treasuryId) {
    return { kind: 'failed', classification: 'setup', reason: 'the account was never funded, so it holds nothing to send' };
  }

  try {
    await session.cosigners[0].midenClient.sync();
    const before = await heldBalance(session);
    if (before <= P2ID_AMOUNT) {
      return {
        kind: 'failed',
        classification: 'setup',
        reason: `the account holds ${before}, which is not enough to send ${P2ID_AMOUNT} and pay the fee`,
      };
    }

    const proposal = await proposeWhenSettled(() =>
      session.multisig!.createP2idProposal(session.treasuryId!, session.faucetId!, P2ID_AMOUNT),
    );
    session.proposalId = proposal.id;
    session.balanceBeforeSend = before;
    session.sentAmount = P2ID_AMOUNT;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing the transfer failed: ${String(error)}`,
    };
  }
}

/**
 * Asserts the sent assets left the vault.
 *
 * Bounded rather than exact: the transaction pays its fee out of the same
 * asset, so the account gives up at least what it sent.
 */
export async function assertAssetSent(_context: ActionContext, scenarioId: string): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || session.balanceBeforeSend === undefined || session.sentAmount === undefined) {
    return { kind: 'failed', classification: 'setup', reason: 'no transfer was proposed in this scenario' };
  }

  try {
    await session.cosigners[0].midenClient.sync();
    await session.multisig.syncState();
    const after = await heldBalance(session);
    const floor = session.balanceBeforeSend - session.sentAmount;
    if (after > floor) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the account holds ${after} after sending ${session.sentAmount}, but no more than ${floor} was expected`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading the balance after the transfer failed: ${String(error)}`,
    };
  }
}

/**
 * Proposes requiring every signer for one procedure, leaving the account's own
 * threshold alone. The override is what makes per-procedure policy testable:
 * the account stays 2-of-3 while that one procedure needs 3.
 */
export async function setProcedureThreshold(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }

  try {
    const signers = await currentSignerSet(session);
    const target = signers.length;
    if (target === session.threshold) {
      return { kind: 'skipped', reason: 'an override equal to the account threshold proves nothing' };
    }

    const proposal = await proposeWhenSettled(() =>
      session.multisig!.createUpdateProcedureThresholdProposal(OVERRIDE_PROCEDURE, target),
    );
    session.proposalId = proposal.id;
    session.expectedProcedure = OVERRIDE_PROCEDURE;
    session.expectedProcedureThreshold = target;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing the procedure threshold override failed: ${String(error)}`,
    };
  }
}

/** Asserts the override landed and the account's own threshold did not move. */
export async function assertProcedureThreshold(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.expectedProcedure || session.expectedProcedureThreshold === undefined) {
    return { kind: 'failed', classification: 'setup', reason: 'no override was proposed in this scenario' };
  }

  try {
    await session.cosigners[0].midenClient.sync();
    await session.multisig.syncState();

    const account = await session.multisig.getStoreAccount();
    const detected = AccountInspector.fromAccount(account);
    const actual = detected.procedureThresholds.get(session.expectedProcedure as never);
    if (actual !== session.expectedProcedureThreshold) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the override for ${session.expectedProcedure} is ${actual ?? 'absent'} but ${session.expectedProcedureThreshold} was expected`,
      };
    }
    // The account threshold this assertion expects, not the one the account
    // started with: a scenario may change it after setting the override, and
    // that the override outlives such a change is the point of doing both.
    const expectedAccountThreshold = session.expectedThreshold ?? session.threshold;
    if (detected.threshold !== expectedAccountThreshold) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the account threshold is ${detected.threshold} but ${expectedAccountThreshold} was expected; an override must not change it`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading back the procedure threshold failed: ${String(error)}`,
    };
  }
}


/**
 * Asserts the removed signer is actually evicted, not merely delisted.
 *
 * Serving a stale signer set and still honouring the removed key are different
 * failures: the first misleads a reader, the second means the removal did not
 * take effect and a compromised cosigner cannot be evicted. This separates
 * them by having the removed key attempt an authenticated call of its own.
 */
export async function assertRemovedSignerRefused(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.accountId || !session.departed) {
    return { kind: 'failed', classification: 'setup', reason: 'no signer was removed in this scenario' };
  }

  const departed = session.departed;
  try {
    const reloaded = await departed.multisigClient.load(session.accountId, departed.signer);
    const served = normalizeCommitments(await reloaded.getSignerPublicKeyCommitments());
    const stillListed = served.includes(departed.signer.commitment.toLowerCase());
    return {
      kind: 'failed',
      classification: 'product',
      reason:
        `GUARDIAN still authenticates the removed signer ${departed.signer.commitment}` +
        `, and it ${stillListed ? 'still appears in' : 'is absent from'} the signer set GUARDIAN serves`,
    };
  } catch (error) {
    const message = String(error);
    // Refused is the point. Anything other than an authorization refusal is
    // not evidence of eviction, so it is reported rather than counted.
    if (/not an authorized signer|unauthorized|authentication|403|401/i.test(message)) {
      return { kind: 'passed' };
    }
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the removed signer was refused, but not as unauthorized: ${message}`,
    };
  }
}

/** The label a producer chooses for a proposal type the SDK does not model. */
const CUSTOM_PROPOSAL_TYPE = 'qualification_probe';

/** The label as this SDK reports it, whatever bucket it was filed under. */
function labelOf(metadata: { proposalType: string; rawProposalType?: string }): string {
  return metadata.proposalType === 'custom'
    ? (metadata.rawProposalType ?? 'custom')
    : metadata.proposalType;
}

/**
 * Proposes a transaction the SDK has no type for, the way a producer does.
 *
 * Every other scenario proposes through the typed API, so all of them exercise
 * the built-in proposal types and none of them exercise the producer path
 * (issue #266): serialized Miden transaction bytes plus a label the SDK has
 * never heard of. That path is the unbounded one, and an integration built on
 * it would break without this suite noticing.
 *
 * The transaction itself is an ordinary P2ID send, chosen because its
 * correctness is already covered elsewhere. What is under test is the label
 * surviving the round trip, not the payment. Mirrors `create_custom_proposal`
 * in the Rust driver.
 */
export async function createCustomProposal(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  if (!session.faucetId || !session.treasuryId) {
    return { kind: 'failed', classification: 'setup', reason: 'the account was never funded, so it holds nothing to send' };
  }

  try {
    await session.cosigners[0].midenClient.sync();

    // Built and serialized here rather than through the typed API, because
    // producer-supplied bytes are the thing being qualified.
    const { request } = buildP2idTransactionRequest(
      session.accountId,
      session.treasuryId,
      session.faucetId,
      P2ID_AMOUNT,
    );
    const bytes = request.serialize();

    const proposal = await proposeWhenSettled(() =>
      session.multisig!.createCustomProposal(bytes, CUSTOM_PROPOSAL_TYPE),
    );
    const label = labelOf(proposal.metadata);
    if (label !== CUSTOM_PROPOSAL_TYPE) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the proposal came back labelled ${label}, not ${CUSTOM_PROPOSAL_TYPE}`,
      };
    }

    session.proposalId = proposal.id;
    session.customNonce = Number(proposal.nonce);
    session.customRequest = bytes;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing a ${CUSTOM_PROPOSAL_TYPE} transaction failed: ${String(error)}`,
    };
  }
}

/**
 * Confirms GUARDIAN stored and serves the producer's own label.
 *
 * The label is the whole contract of the producer API: a GUARDIAN that accepted
 * the proposal but returned it as `custom`, or as one of its own built-ins,
 * would leave every producer unable to tell its proposals apart while every
 * other signal looked healthy.
 *
 * Read from GUARDIAN's answer rather than from the client that made it, because
 * the client's own copy would agree with itself. That forces a different call
 * than the Rust leg makes: `listProposals` is a local cache, and `syncProposals`
 * does fetch from GUARDIAN but keeps the local metadata for a proposal this
 * client created, so neither one can answer the question here. The wire answer
 * is decoded through the SDK's own codec, so what is asserted is still what a
 * consumer would read, not a raw field this harness picked out.
 */
export async function assertCustomProposalType(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  if (!session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }
  const proposalId = normalizeWord(session.proposalId);

  try {
    const deltas =
      await session.cosigners[0].multisigClient.guardianClient.getDeltaProposals(session.accountId);
    const mine = deltas.find(
      (delta) =>
        normalizeWord(computeCommitmentFromTxSummary(delta.deltaPayload.txSummary.data)) ===
        proposalId,
    );
    if (!mine) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `GUARDIAN does not list the custom proposal ${session.proposalId}`,
      };
    }

    const label = labelOf(ProposalMetadataCodec.fromGuardian(mine.deltaPayload.metadata));
    if (label !== CUSTOM_PROPOSAL_TYPE) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `GUARDIAN serves the proposal as ${label}, not as the producer's ${CUSTOM_PROPOSAL_TYPE}`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading the custom proposal back from GUARDIAN failed: ${String(error)}`,
    };
  }
}

/**
 * The advice-map key a signature is filed under, which is how an integration
 * finds the entry it has to inject.
 *
 * Both Words are built for this call and never reused: hashing consumes the
 * element array it is handed, and the SDK's own signature helper builds a fresh
 * Word per entry for the same reason.
 */
function adviceKeyFor(commitmentHex: string, messageHex: string): Word {
  return Poseidon2.hashElements(
    new FeltArray([
      ...Word.fromHex(normalizeWord(commitmentHex)).toFelts(),
      ...Word.fromHex(normalizeWord(messageHex)).toFelts(),
    ]),
  );
}

/**
 * Assembles the execution advice a producer integration needs, which is where
 * the SDK's responsibility for a custom proposal ends.
 *
 * A custom proposal is deliberately not executed by `executeProposal`: the SDK
 * cannot rebuild an arbitrary producer transaction, so it hands back the
 * cosigner signatures and GUARDIAN's acknowledgement, and the integration
 * injects them into its own request and submits with its own Miden client. This
 * scenario stops at that boundary rather than reimplementing an integration.
 *
 * What the boundary is worth asserting for: preparing re-executes the producer's
 * own bytes at the proposal's anchored block and refuses unless they reproduce
 * the signed commitment. So a pass here means the label survived, the threshold
 * was met, and the bytes still match what was signed.
 *
 * The Rust leg checks the returned advice is not empty. This SDK returns an
 * `AdviceMap`, which exposes no size, so the check is made by looking up the one
 * entry an integration cannot proceed without: GUARDIAN's acknowledgement, under
 * the key the producer's own transaction will read it from.
 */
export async function prepareCustomExecution(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  if (!session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }
  if (!session.customRequest) {
    return { kind: 'failed', classification: 'setup', reason: 'no custom proposal was created in this scenario' };
  }

  try {
    const delta = await session.cosigners[0].multisigClient.guardianClient.getDeltaProposal(
      session.accountId,
      normalizeWord(session.proposalId),
    );
    const signedCommitment = normalizeWord(
      TransactionSummary.deserialize(base64ToUint8Array(delta.deltaPayload.txSummary.data))
        .toCommitment()
        .toHex(),
    );

    const advice = await session.multisig.prepareCustomExecution(
      session.proposalId,
      session.customRequest,
    );

    const acknowledgement = advice.get(
      adviceKeyFor(session.multisig.guardianCommitment, signedCommitment),
    );
    if (!acknowledgement) {
      return {
        kind: 'failed',
        classification: 'product',
        reason:
          'the prepared advice carries no GUARDIAN acknowledgement, so an integration would have nothing to inject',
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `preparing the custom execution failed: ${String(error)}`,
    };
  }
}

/**
 * How long an abandoned candidate may take to resolve. The quarantine is a
 * short wall-clock minimum plus a couple of at-base observations, so this is
 * generous rather than tight.
 */
const ABANDON_DEADLINE_MS = 120_000;

/**
 * How long the discarded proposal may stay in this client's listing.
 *
 * Longer than one call on purpose: unlike the Rust client, this one keeps a
 * proposal GUARDIAN has acknowledged until two consecutive listings omit it, so
 * a single sync after the discard proves nothing either way.
 */
const LISTING_PRUNE_DEADLINE_MS = 60_000;

/**
 * The negative control for how every other scenario asserts completion.
 *
 * Completion is chain confirmation plus a canonical delta, and not "the proposal
 * left the pending set", because canonicalization removes a discarded delta
 * exactly as it removes a successful one. Every other scenario exercises the
 * positive side of that rule. This is the negative: it produces a real discard
 * and checks nothing serves it as live, so the rule is falsified by experiment
 * rather than only correct by construction.
 *
 * The candidate comes from the producer API, which is the one path that
 * separates acknowledgement from submission: preparing pushes the delta to
 * obtain GUARDIAN's acknowledgement, and submitting is a separate call the
 * integration makes. Stopping in between leaves a candidate that can never land,
 * which is precisely the state the abandon API exists for, and it reaches that
 * state through supported calls rather than by forcing GUARDIAN into it.
 *
 * Nothing here reaches Miden, so the account stays at the candidate's base and
 * the abandon resolves through its designed at-base path rather than through
 * retry exhaustion.
 */
export async function abandonAndAssertHidden(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  if (session.customNonce === undefined) {
    return { kind: 'failed', classification: 'setup', reason: 'no custom proposal was prepared in this scenario' };
  }
  if (!session.proposalId) {
    return { kind: 'failed', classification: 'setup', reason: 'no proposal has been created in this scenario' };
  }
  const nonce = session.customNonce;
  const proposalId = session.proposalId;

  // Established before abandoning, or the check afterwards means nothing: a
  // proposal already gone from the pending set would satisfy it without the
  // discard having hidden anything.
  try {
    const pending = await session.multisig.syncProposals();
    if (!pending.some((proposal) => proposal.id === proposalId)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the proposal ${proposalId} is not pending before the abandon, so its absence afterwards would prove nothing`,
      };
    }
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `listing proposals before the abandon failed: ${String(error)}`,
    };
  }

  try {
    await session.multisig.abandonCandidate(nonce);
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `abandoning the candidate at nonce ${nonce} failed: ${String(error)}`,
    };
  }

  const abandonDeadline = Date.now() + ABANDON_DEADLINE_MS;
  let wait = POLL_START_MS;
  let state = 'never answered';
  for (;;) {
    try {
      const status = await session.multisig.abandonStatus(nonce);
      if (status === 'abandoned') break;
      if (status === 'landed') {
        return {
          kind: 'failed',
          classification: 'product',
          reason:
            'the candidate canonicalized, so nothing was discarded to look for; this scenario never submits, so GUARDIAN saw a transaction it should not have',
        };
      }
      state = status;
    } catch (error) {
      state = String(error);
    }
    if (Date.now() >= abandonDeadline) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the abandoned candidate at nonce ${nonce} was still ${state} after ${ABANDON_DEADLINE_MS / 1000}s`,
      };
    }
    wait = await backoff(wait);
  }

  // The discard is only safe while it is invisible to what a client reads by
  // default. A discarded delta still listed as a pending proposal is the shape
  // that makes "it left the pending set" look like completion.
  const pruneDeadline = Date.now() + LISTING_PRUNE_DEADLINE_MS;
  let stillListed = true;
  let listingError = '';
  wait = POLL_START_MS;
  for (;;) {
    try {
      const listed = await session.multisig.syncProposals();
      stillListed = listed.some((proposal) => proposal.id === proposalId);
      listingError = '';
      if (!stillListed) break;
    } catch (error) {
      listingError = String(error);
    }
    if (Date.now() >= pruneDeadline) break;
    wait = await backoff(wait);
  }
  if (listingError) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `listing proposals after the discard failed: ${listingError}`,
    };
  }
  if (stillListed) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the delta at nonce ${nonce} was discarded but its proposal ${proposalId} is still listed as pending`,
    };
  }

  // It must not have moved the account. Reading state back is what a client does
  // next, and it is the other way a discard could pass for a completion.
  try {
    await session.multisig.verifyStateCommitment();
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the account does not agree with chain after a discarded delta: ${String(error)}`,
    };
  }

  // The rule itself, not only its symptom. Everything above shows the discard is
  // invisible to what a client reads by default, which is worth having, but the
  // code that has to tell a discard from a success is `waitForExecution`: every
  // other live scenario trusts its verdict. Ask it about a delta that really was
  // discarded, and it must say so rather than read the empty pending set as
  // completion.
  const completion = await waitForExecution(session, proposalId, { kind: 'nonce', nonce });
  switch (completion.kind) {
    case 'discarded':
      return { kind: 'passed' };
    case 'confirmed':
      return {
        kind: 'failed',
        classification: 'product',
        reason:
          'the completion check calls a discarded delta confirmed, so every scenario that trusts it would read an abandoned candidate as a successful execution',
      };
    case 'pending':
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the completion check still calls the discarded delta pending after its deadline: ${completion.reason}`,
      };
  }
}

/**
 * Far enough ahead that the note stays locked for the life of the run, without
 * needing the chain tip to compute it. The assets stay in the note; these
 * accounts are ephemeral and their residue is accepted rather than swept.
 */
const P2IDE_TIMELOCK_HEIGHT = 4_000_000_000;

/**
 * Sends a timelocked note to the account itself.
 *
 * P2ID is covered; P2IDE is the same flow with a height attached, and nothing
 * exercised it. Self-addressed on purpose: the timelock is only observable from
 * the recipient's side, and sending to a counterparty this scenario does not
 * drive would leave nothing to assert against.
 */
export async function sendP2ide(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }
  if (!session.faucetId) {
    return { kind: 'failed', classification: 'setup', reason: 'the account was never funded, so it holds nothing' };
  }

  try {
    await session.cosigners[0].midenClient.sync();
    const before = await heldBalance(session);
    if (before <= P2ID_AMOUNT) {
      return {
        kind: 'failed',
        classification: 'setup',
        reason: `the account holds ${before}, which is not enough to send ${P2ID_AMOUNT} and pay the fee`,
      };
    }

    const proposal = await proposeWhenSettled(() =>
      session.multisig!.createP2idProposal(session.accountId!, session.faucetId!, P2ID_AMOUNT, {
        timelockHeight: P2IDE_TIMELOCK_HEIGHT,
      }),
    );
    session.proposalId = proposal.id;
    session.balanceBeforeSend = before;
    session.sentAmount = P2ID_AMOUNT;
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `proposing the timelocked send failed: ${String(error)}`,
    };
  }
}

/**
 * Confirms the height on the note is doing something.
 *
 * A P2IDE note whose timelock were dropped, or encoded as the on-chain "no
 * constraint" zero, would be indistinguishable from a plain P2ID at every other
 * point in this flow: the transaction executes, the balance moves, the note
 * lands. The difference shows only here, and only as the pair of answers below.
 * Committed alone would pass for a P2ID; not-consumable alone would pass for a
 * note that never arrived.
 */
export async function assertP2ideTimelocked(
  _context: ActionContext,
  scenarioId: string,
): Promise<ActionOutcome> {
  const session = sessions.get(scenarioId);
  if (!session?.multisig || !session.accountId) {
    return { kind: 'failed', classification: 'setup', reason: 'no account has been created in this scenario' };
  }

  try {
    // Read from the sending side, which is where this client records the note.
    // The Rust leg reads the recipient side, and the account is its own
    // recipient, so the two should agree. They do not: this client's input-note
    // store never takes in a note the account sent itself, while the Rust
    // client's does, and neither a status listing nor an availability listing
    // showed it after three minutes. What that costs is one half of the pair,
    // not the pair itself: a committed output note is on chain, which is the
    // same "it landed" the Rust leg asserts, and the other half still asks this
    // account what it can consume. The divergence is recorded in
    // docs/QUALIFICATION.md rather than worked around silently.
    //
    // Polled because the note lands in the block the execution landed in and
    // the client only sees it once a sync covers that block, which is tolerance
    // for chain lag rather than a weaker assertion.
    const deadline = Date.now() + NOTE_ARRIVAL_DEADLINE_MS;
    let wait = POLL_START_MS;
    let landed: string[] = [];
    for (;;) {
      try {
        await session.cosigners[0].midenClient.sync();
        await session.multisig.syncState();
        const sent = await session.cosigners[0].midenClient.notes.listSent({
          status: 'committed',
        });
        landed = sent.map((record) => record.id().toString());
      } catch {
        // A sync that loses a race with block production is retried, not fatal.
      }
      if (landed.length > 0) break;
      if (Date.now() >= deadline) {
        return {
          kind: 'failed',
          classification: 'product',
          reason: `the timelocked note never committed on chain within ${NOTE_ARRIVAL_DEADLINE_MS / 1000}s, so the timelock cannot be read`,
        };
      }
      wait = await backoff(wait);
    }
    const committedIds = new Set(landed);

    const consumable = await session.multisig.getConsumableNotes();
    const unlocked = consumable.filter((note) => committedIds.has(note.id)).map((note) => note.id);
    if (unlocked.length > 0) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `a note timelocked to block ${P2IDE_TIMELOCK_HEIGHT} is already consumable: ${unlocked.join(', ')}`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading the timelocked note back failed: ${String(error)}`,
    };
  }
}
