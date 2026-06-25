/**
 * End-to-end operator walkthrough: connect (login) → show permissions →
 * list accounts → pause the first one → unpause it. Use this to watch the
 * whole flow at once; the focused per-operation scripts (list-accounts.ts,
 * list-deltas.ts, list-proposals.ts, pause.ts, unpause.ts) share the same
 * `connect()` helper.
 *
 *   npm install
 *   GUARDIAN_URL=http://localhost:3000 \
 *   GUARDIAN_OPERATOR_PRIVATE_KEY=<hex from generate-operator-key.ts> \
 *   npm run demo
 *
 * Listing needs `dashboard:read`; the pause/unpause step needs `accounts:pause`.
 */
import { connect } from './session.ts';

const { client, commitment } = await connect();
console.log(`logged in — operator commitment: ${commitment}`);

const { operatorId, permissions } = await client.getSession();
console.log(`operator ${operatorId} — permissions: ${permissions.length ? permissions.join(', ') : '(none)'}`);

const { items } = await client.listAccounts();
console.log(`\naccounts: ${items.length}`);
for (const a of items.slice(0, 10)) {
  const state = a.pausedAt ? `PAUSED (${a.pausedReason ?? 'no reason'})` : a.stateStatus;
  console.log(`  ${a.accountId}  ${state}`);
}

const target = items[0];
if (!target) {
  console.log('\nno accounts yet — skipping the pause/unpause demo.');
} else {
  console.log(`\npausing ${target.accountId} …`);
  try {
    const paused = await client.pauseAccount(target.accountId, 'operator-demo: compliance hold');
    console.log(`  paused at ${paused.pausedAt} — reason: ${paused.pausedReason}`);
    const unpaused = await client.unpauseAccount(target.accountId, 'operator-demo: cleared');
    console.log(`  unpaused ${unpaused.accountId}`);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    console.log(`  pause/unpause failed — does this operator have accounts:pause? (${message})`);
  }
}

await client.logout();
console.log('\ndone.');
