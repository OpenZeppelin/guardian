/**
 * List in-flight multisig proposals and their signature progress. With an
 * account id, lists that account's proposals; without one, the global feed.
 *   GUARDIAN_OPERATOR_PRIVATE_KEY=<hex> npx tsx list-proposals.ts [accountId]
 * Requires `dashboard:read`.
 */
import { connect } from './session.ts';

const accountId = process.argv[2];
const { client } = await connect();

const { items } = accountId
  ? await client.listAccountProposals(accountId)
  : await client.listGlobalProposals();

console.log(`proposals${accountId ? ` for ${accountId}` : ' (global)'}: ${items.length}`);
for (const p of items) {
  const who = p.accountId ? `${p.accountId}  ` : '';
  const kind = p.proposalType ?? '—';
  console.log(
    `  ${who}nonce=${p.nonce}  ${kind}  sigs=${p.signaturesCollected}/${p.signaturesRequired}  proposer=${p.proposerId}`,
  );
}
