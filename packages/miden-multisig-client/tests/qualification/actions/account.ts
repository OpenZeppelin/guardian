import { GuardianHttpClient } from '@openzeppelin/guardian-client';

import { FalconSigner } from '../../../src/signers/falcon.js';
import { loadServerFixtures, type ServerFixtures } from '../fixtures.js';
import type { ActionContext, ActionOutcome } from '../runner.js';

interface Session {
  readonly guardian: GuardianHttpClient;
  readonly fixtures: ServerFixtures;
}

function openSession(context: ActionContext): Session {
  const fixtures = loadServerFixtures();
  const guardian = new GuardianHttpClient(context.httpEndpoint);
  guardian.setSigner(new FalconSigner(fixtures.signerKey));
  return { guardian, fixtures };
}

function setupFailure(error: unknown): ActionOutcome {
  return {
    kind: 'failed',
    classification: 'setup',
    reason: `cannot load the server fixtures: ${String(error)}`,
  };
}

export async function register(context: ActionContext): Promise<ActionOutcome> {
  let session: Session;
  try {
    session = openSession(context);
  } catch (error) {
    return setupFailure(error);
  }

  const { account } = session.fixtures;
  try {
    await session.guardian.configure({
      accountId: session.fixtures.accountId,
      auth: { MidenFalconRpo: { cosigner_commitments: [...session.fixtures.cosignerCommitments] } },
      initialState: { data: account.data, accountId: account.account_id },
    });
    return { kind: 'passed' };
  } catch (error) {
    const code = (error as { code?: string | null }).code;
    if (code === 'account_already_configured') return { kind: 'passed' };
    return {
      kind: 'failed',
      classification: 'product',
      reason: `registering the fixture account failed${code ? ` with ${code}` : ''}: ${String(error)}`,
    };
  }
}

/**
 * Whether two commitments name the same state, whatever their spelling.
 *
 * GUARDIAN's rendering and the fixture's are both hex words, but nothing
 * guarantees the same case or the same `0x`, and a comparison that tripped on
 * either would fail for a reason that is not a defect. Mirrors
 * `same_commitment` in the Rust driver.
 */
export function sameCommitment(left: string, right: string): boolean {
  const canonical = (value: string): string =>
    value.trim().replace(/^0x/i, '').toLowerCase();
  const [a, b] = [canonical(left), canonical(right)];
  return a.length > 0 && a === b;
}

/**
 * Reads the account back through GUARDIAN and checks the commitment it reports
 * is the one the registered state carries.
 *
 * Compared against the fixture's own commitment rather than merely required to
 * be present. A GUARDIAN that stored a corrupted or stale state, or served
 * another account's, would answer with a perfectly well-formed commitment, and
 * a presence check would call that a pass on a required scenario.
 */
export async function verifyCommitment(context: ActionContext): Promise<ActionOutcome> {
  let session: Session;
  try {
    session = openSession(context);
  } catch (error) {
    return setupFailure(error);
  }

  try {
    const state = await session.guardian.getState(session.fixtures.accountId);
    if (!state?.commitment) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: 'GUARDIAN returned an account with no commitment',
      };
    }
    if (!sameCommitment(state.commitment, session.fixtures.initialCommitment)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason:
          `GUARDIAN reports commitment ${state.commitment} for the fixture account, but the ` +
          `state it was registered with carries ${session.fixtures.initialCommitment}`,
      };
    }
    if (state.accountId && state.accountId !== session.fixtures.accountId) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `GUARDIAN returned account ${state.accountId} for a request about ${session.fixtures.accountId}`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `reading the registered account back failed: ${String(error)}`,
    };
  }
}
