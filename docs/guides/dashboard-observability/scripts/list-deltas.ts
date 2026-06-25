/**
 * List deltas (applied state transitions). With an account id, lists that
 * account's deltas; without one, the global feed across all accounts.
 *   GUARDIAN_OPERATOR_PRIVATE_KEY=<hex> npx tsx list-deltas.ts [accountId]
 * Requires `dashboard:read`.
 */
import { connect } from './session.ts';

const accountId = process.argv[2];
const { client } = await connect();

const { items } = accountId
  ? await client.listAccountDeltas(accountId)
  : await client.listGlobalDeltas();

console.log(`deltas${accountId ? ` for ${accountId}` : ' (global)'}: ${items.length}`);
for (const d of items) {
  const who = d.accountId ? `${d.accountId}  ` : '';
  const kind = d.proposalType ?? d.category ?? '—';
  console.log(`  ${who}nonce=${d.nonce}  ${d.status}  ${kind}  ${d.statusTimestamp}`);
}
