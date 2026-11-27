import { GuardianHttpClient, type ModuleRefusal } from '@openzeppelin/guardian-client';

import { FalconSigner } from '../../../src/signers/falcon.js';
import { loadServerFixtures, readThresholdDelta } from '../fixtures.js';
import { setAccountModules } from './operator.js';
import type { ActionContext, ActionOutcome } from '../runner.js';

/** The instance the stack's catalog declares for rules a scenario assigns. */
export const GATE = 'qual-gate';
export const CAP = 'qual-cap';
export const RECIPIENT = 'qual-recipient';
export const REGISTER = 'qual-register';

/**
 * A signer commitment nobody holds. Checked against every signer, an allowlist
 * of only this refuses every transaction of the fixture account.
 */
const UNLISTED_SIGNER = '0x1111111111111111111111111111111111111111111111111111111111111111';

/** The policy module refusal `error` carries, or `null`. */
export function refusalOf(error: unknown): ModuleRefusal | null {
  const carrier = error as { moduleRefusal?: () => ModuleRefusal | null };
  return typeof carrier?.moduleRefusal === 'function' ? carrier.moduleRefusal() : null;
}

/** `passed` when `attempt` is refused by `module` for `reasonCode`. */
export async function expectDenied(
  what: string,
  attempt: () => Promise<unknown>,
  module: string,
  reasonCode: string,
): Promise<ActionOutcome> {
  try {
    await attempt();
    return { kind: 'failed', classification: 'product', reason: `${what} was accepted while ${module} refuses it` };
  } catch (error) {
    const refusal = refusalOf(error);
    if (refusal?.kind === 'denied' && refusal.module === module && refusal.reasonCode === reasonCode) {
      return { kind: 'passed' };
    }
    return {
      kind: 'failed',
      classification: 'product',
      reason: refusal
        ? `${what} was refused as ${JSON.stringify(refusal)}, not by ${module} for ${reasonCode}`
        : `${what} failed without a module refusal: ${String(error)}`,
    };
  }
}

/**
 * The registration hook over HTTP: `qual-register`, a signer policy on
 * `before_account_registered` only, assigned to the fixture account. Checking
 * every signer against an allowlist naming none of them, it refuses a
 * re-registration of the account with its own signers. Checking only added
 * signers, it allows the same re-registration, since every signer is already in
 * the stored state the hook receives as `before`. The assignment is cleared on
 * every path. Mirrors `assert_registration_refusal` in the Rust driver.
 */
export async function assertRegistrationRefusal(context: ActionContext): Promise<ActionOutcome> {
  let configure: () => Promise<unknown>;
  let accountId: string;
  try {
    const fixtures = loadServerFixtures();
    const guardian = new GuardianHttpClient(context.httpEndpoint);
    guardian.setSigner(new FalconSigner(fixtures.signerKey));
    accountId = fixtures.accountId;
    configure = () =>
      guardian.configure({
        accountId: fixtures.accountId,
        auth: { MidenFalconRpo: { cosigner_commitments: [...fixtures.cosignerCommitments] } },
        initialState: { data: fixtures.account.data, accountId: fixtures.account.account_id },
      });
  } catch (error) {
    return { kind: 'failed', classification: 'setup', reason: `cannot load the server fixtures: ${String(error)}` };
  }

  const outcome = await registrationDecisions(context, accountId, configure);
  const cleared = await setAccountModules(context, accountId, []);
  if (outcome.kind !== 'passed') return outcome;
  return cleared ?? outcome;
}

async function registrationDecisions(
  context: ActionContext,
  accountId: string,
  configure: () => Promise<unknown>,
): Promise<ActionOutcome> {
  const everySigner = await setAccountModules(context, accountId, [
    { moduleId: REGISTER, params: { check: 'all', allowlist: [UNLISTED_SIGNER] } },
  ]);
  if (everySigner) return everySigner;
  const refused = await expectDenied(
    'a re-registration checked for every signer',
    configure,
    REGISTER,
    'signer_not_allowed',
  );
  if (refused.kind !== 'passed') return refused;

  const addedOnly = await setAccountModules(context, accountId, [
    { moduleId: REGISTER, params: { check: 'added', allowlist: [UNLISTED_SIGNER] } },
  ]);
  if (addedOnly) return addedOnly;
  try {
    await configure();
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `a re-registration that adds no signer, checked for added signers only, was refused: ${String(error)}`,
    };
  }
}

/**
 * An assigned in-process module refuses the fixture proposal over HTTP, at the
 * dry-run check and at creation, with its id and reason code, and the same
 * proposal passes once the assignment is cleared. Mirrors `assert_refusal` in
 * the Rust driver.
 */
export async function assertRefusal(context: ActionContext): Promise<ActionOutcome> {
  let guardian: GuardianHttpClient;
  let accountId: string;
  let delta: ReturnType<typeof readThresholdDelta>;
  try {
    const fixtures = loadServerFixtures();
    guardian = new GuardianHttpClient(context.httpEndpoint);
    guardian.setSigner(new FalconSigner(fixtures.signerKey));
    accountId = fixtures.accountId;
    delta = readThresholdDelta();
  } catch (error) {
    return { kind: 'failed', classification: 'setup', reason: `cannot load the server fixtures: ${String(error)}` };
  }
  const request = {
    accountId,
    nonce: delta.nonce,
    deltaPayload: {
      txSummary: { data: delta.delta_payload.data },
      signatures: [],
      metadata: { proposalType: 'qualification' },
    },
  };

  const assigned = await setAccountModules(context, accountId, [
    { moduleId: GATE, params: { check: 'all', allowlist: [UNLISTED_SIGNER] } },
  ]);
  if (assigned) return assigned;

  let outcome = await expectDenied(
    'the proposal check',
    () => guardian.checkDeltaProposal(request),
    GATE,
    'signer_not_allowed',
  );
  if (outcome.kind === 'passed') {
    outcome = await expectDenied('the proposal', () => guardian.pushDeltaProposal(request), GATE, 'signer_not_allowed');
  }

  const cleared = await setAccountModules(context, accountId, []);
  if (outcome.kind !== 'passed') return outcome;
  if (cleared) return cleared;

  try {
    await guardian.checkDeltaProposal(request);
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the proposal check, once the module was unassigned, was refused: ${String(error)}`,
    };
  }
}
