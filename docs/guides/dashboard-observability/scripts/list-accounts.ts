/**
 * List all accounts Guardian knows about, newest activity first.
 *   npm install && GUARDIAN_OPERATOR_PRIVATE_KEY=<hex> npx tsx list-accounts.ts
 * Requires `dashboard:read`.
 */
import { connect } from './session.ts';

const { client } = await connect();
const { items } = await client.listAccounts();

console.log(`accounts: ${items.length}`);
for (const a of items) {
  const state = a.pausedAt ? `PAUSED (${a.pausedReason ?? 'no reason'})` : a.stateStatus;
  console.log(`  ${a.accountId}  ${state}  signers=${a.authorizedSignerCount}  updated=${a.updatedAt}`);
}
