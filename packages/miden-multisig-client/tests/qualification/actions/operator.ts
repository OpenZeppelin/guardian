import { GuardianOperatorHttpClient } from '@openzeppelin/guardian-operator-client';
import { Word } from '@miden-sdk/miden-sdk';

import { fetchWithCookieJar } from '../cookieJar.js';
import { loadServerFixtures, operatorKey } from '../fixtures.js';
import { bytesToHex } from '../../../src/utils/encoding.js';
import type { ActionContext, ActionOutcome } from '../runner.js';

// The client prefixes `dashboard/` itself for the data routes, while the auth
// routes sit at the server root, so the base URL is the root with a trailing
// slash rather than the dashboard path.
function operatorBaseUrl(context: ActionContext): string {
  return `${context.httpEndpoint.replace(/\/$/, '')}/`;
}

/**
 * Establishes an operator session: request a challenge for the operator's
 * commitment, sign the digest it returns, and exchange it for a session cookie.
 */
// Challenge and verify are rate limited per operator commitment, so sessions
// are reused across scenarios rather than re-established for every action.
const sessions = new Map<string, GuardianOperatorHttpClient>();

async function login(
  context: ActionContext,
  which: 'reader' | 'restricted',
  fresh = false,
): Promise<{ client: GuardianOperatorHttpClient } | { failure: ActionOutcome }> {
  const cached = sessions.get(which);
  if (cached && !fresh) return { client: cached };

  const key = operatorKey(which);
  const client = new GuardianOperatorHttpClient({
    baseUrl: operatorBaseUrl(context),
    fetch: fetchWithCookieJar(),
  });

  try {
    const { challenge } = await client.challenge(key.commitment);
    const signature = bytesToHex(
      key.secretKey.sign(Word.fromHex(challenge.signingDigest)).serialize().slice(1),
    );
    await client.verify({ commitment: key.commitment, signature });
    if (!fresh) sessions.set(which, client);
    return { client };
  } catch (error) {
    return {
      failure: {
        kind: 'failed',
        classification: 'product',
        reason: `operator login failed for the ${which} operator: ${String(error)}`,
      },
    };
  }
}

/**
 * Pauses or unpauses an account as the operator that holds `accounts:pause`.
 *
 * Exported so the live pause scenario drives the same operator path the
 * deterministic one does, rather than reimplementing it and drifting on what
 * pausing an account means.
 */
export async function setAccountPaused(
  context: ActionContext,
  accountId: string,
  paused: boolean,
): Promise<ActionOutcome | null> {
  const result = await login(context, 'reader');
  if ('failure' in result) return result.failure;

  try {
    if (paused) {
      await result.client.pauseAccount(accountId, 'qualification: paused-account scenario');
    } else {
      await result.client.unpauseAccount(accountId);
    }
    return null;
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `${paused ? 'pause' : 'unpause'} was refused: ${String(error)}`,
    };
  }
}

export async function assertSession(context: ActionContext): Promise<ActionOutcome> {
  const result = await login(context, 'reader');
  if ('failure' in result) return result.failure;

  try {
    const session = await result.client.getSession();
    if (!session?.operatorId) {
      return { kind: 'failed', classification: 'product', reason: 'the session carries no operator identity' };
    }
    if (!session.permissions?.includes('dashboard:read')) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the reader operator's session lacks dashboard:read: ${JSON.stringify(session.permissions)}`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return { kind: 'failed', classification: 'product', reason: `inspecting the session failed: ${String(error)}` };
  }
}

export async function assertAccounts(context: ActionContext): Promise<ActionOutcome> {
  const result = await login(context, 'reader');
  if ('failure' in result) return result.failure;
  const fixtures = loadServerFixtures();

  try {
    const accounts = await result.client.listAccounts();
    const listed = accounts.items ?? [];
    if (!listed.some((entry) => entry.accountId === fixtures.accountId)) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `the registered account ${fixtures.accountId} does not appear in the operator account list`,
      };
    }
    const detail = await result.client.getAccount(fixtures.accountId);
    if (detail?.accountId !== fixtures.accountId) {
      return {
        kind: 'failed',
        classification: 'product',
        reason: `account detail returned ${detail?.accountId ?? 'nothing'} for ${fixtures.accountId}`,
      };
    }
    return { kind: 'passed' };
  } catch (error) {
    return { kind: 'failed', classification: 'product', reason: `listing accounts failed: ${String(error)}` };
  }
}

/**
 * An operator holding no permissions must be refused, and refused with a
 * structured code rather than an opaque failure.
 */
export async function assertDenial(context: ActionContext): Promise<ActionOutcome> {
  const result = await login(context, 'restricted');
  if ('failure' in result) return result.failure;

  try {
    await result.client.listAccounts();
    return {
      kind: 'failed',
      classification: 'product',
      reason: 'an operator with no permissions was allowed to list accounts',
    };
  } catch (error) {
    const code = (error as { data?: { code?: string } }).data?.code;
    if (code === 'insufficient_operator_permission') return { kind: 'passed' };
    return {
      kind: 'failed',
      classification: 'product',
      reason: `expected insufficient_operator_permission, got ${code ?? String(error)}`,
    };
  }
}

/**
 * The allowlist is re-read on every challenge and authenticated request, so a
 * permission granted while the server is running must take effect without a
 * restart.
 */
export async function assertAllowlistReload(context: ActionContext): Promise<ActionOutcome> {
  const path = process.env.QUAL_OPERATOR_ALLOWLIST;
  if (!path) {
    return {
      kind: 'skipped',
      reason: 'QUAL_OPERATOR_ALLOWLIST is not set, so the allowlist cannot be mutated',
    };
  }

  const { readFileSync, writeFileSync, renameSync } = await import('node:fs');
  const original = readFileSync(path, 'utf8');
  const restricted = operatorKey('restricted');

  // The server re-reads this file on every request and has no tolerance for a
  // partial one, so a plain write races it: truncate-then-write is visible as a
  // half-written file and the request fails with a 500. Writing a sibling and
  // renaming makes the swap atomic, which is what any tool editing a watched
  // file has to do.
  const writeAllowlist = (contents: string): void => {
    const staged = `${path}.staged`;
    writeFileSync(staged, contents);
    renameSync(staged, path);
  };

  try {
    const entries = JSON.parse(original) as Array<{ public_key: string; permissions: string[] }>;
    const target = entries.find((entry) => entry.public_key === restricted.publicKey);
    if (!target) {
      return {
        kind: 'failed',
        classification: 'setup',
        reason: 'the restricted operator is not present in the allowlist',
      };
    }
    target.permissions = ['dashboard:read'];
    writeAllowlist(`${JSON.stringify(entries, null, 2)}\n`);

    const result = await login(context, 'restricted', true);
    if ('failure' in result) return result.failure;
    await result.client.listAccounts();
    return { kind: 'passed' };
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `the allowlist change did not take effect without a restart: ${String(error)}`,
    };
  } finally {
    writeAllowlist(original);
    sessions.delete('restricted');
  }
}

export async function assertLogout(context: ActionContext): Promise<ActionOutcome> {
  const result = await login(context, 'reader', true);
  if ('failure' in result) return result.failure;

  try {
    await result.client.logout();
  } catch (error) {
    return { kind: 'failed', classification: 'product', reason: `logout failed: ${String(error)}` };
  }

  try {
    await result.client.getSession();
    return {
      kind: 'failed',
      classification: 'product',
      reason: 'the session was still usable after logout',
    };
  } catch {
    return { kind: 'passed' };
  }
}
