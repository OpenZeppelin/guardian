import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';

import { GuardianHttpClient } from '@openzeppelin/guardian-client';

import { AccountInspector } from '../../../src/inspector.js';
import { AccountId } from '@miden-sdk/miden-sdk';

import { bytesToHex } from '../../../src/utils/encoding.js';
import { cosignWithRust } from '../handoff.js';
import { fundAccount } from '../funding.js';
import {
  buildCosigners,
  guardianCommitment,
  shapeOf,
  type LiveSession,
  type Scheme,
} from '../live.js';
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
  session.transferred = BigInt(funded.amount);
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
async function waitForExecution(session: LiveSession, proposalId: string): Promise<Completion> {
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
        if (session.migrating) {
          const state = await session.multisig!.syncState();
          if (state) return { kind: 'confirmed' };
          last = 'the new GUARDIAN does not serve the migrated account';
        } else {
          const history = await session.multisig!.deltaHistory({ limit: 20 });
          const canonical = history.entries.some(
            (entry) => entry.newCommitment && normalizeHex(entry.newCommitment) === commitment,
          );
          if (canonical) return { kind: 'confirmed' };
          last = `no canonical delta carries commitment ${commitment}`;
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

  const completion = await waitForExecution(session, session.proposalId);
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

  const completion = await waitForExecution(session, session.proposalId);
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
