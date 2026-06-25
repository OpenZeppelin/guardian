/**
 * Unpause an account — restores delta/proposal writes.
 *   GUARDIAN_OPERATOR_PRIVATE_KEY=<hex> npx tsx unpause.ts <accountId> [reason]
 * Requires `accounts:pause`. Idempotent (unpausing an active account is a no-op).
 */
import { connect } from './session.ts';

const [, , accountId, ...reasonParts] = process.argv;
const reason = reasonParts.length > 0 ? reasonParts.join(' ') : undefined;
if (!accountId) {
  console.error('usage: npx tsx unpause.ts <accountId> [reason]');
  process.exit(1);
}

const { client } = await connect();
const unpaused = await client.unpauseAccount(accountId, reason);
console.log(`unpaused ${unpaused.accountId}`);
