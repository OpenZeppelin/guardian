/**
 * Pause an account — blocks delta/proposal writes until unpaused.
 *   GUARDIAN_OPERATOR_PRIVATE_KEY=<hex> npx tsx pause.ts <accountId> <reason>
 * Requires `accounts:pause`. Idempotent (re-pausing keeps the original reason).
 */
import { connect } from './session.ts';

const [, , accountId, ...reasonParts] = process.argv;
const reason = reasonParts.join(' ');
if (!accountId || !reason) {
  console.error('usage: npx tsx pause.ts <accountId> <reason>');
  process.exit(1);
}

const { client } = await connect();
const paused = await client.pauseAccount(accountId, reason);
console.log(`paused ${paused.accountId} at ${paused.pausedAt} — reason: ${paused.pausedReason}`);
