/**
 * Guardian operator CLI demo — authenticate and list accounts.
 *
 * First run (no key file): generates a Falcon keypair, writes the secret key
 * to ./operator-key.txt, prints the operators.json entry, and exits.
 *
 * Subsequent runs: reads the key from ./operator-key.txt and authenticates.
 *
 * Usage:
 *   npx tsx list-accounts.ts
 *
 * Point at a different instance:
 *   GUARDIAN_URL=https://guardian.example npx tsx list-accounts.ts
 */

import { readFileSync, writeFileSync, existsSync } from 'fs';
import { GuardianOperatorHttpClient } from '@openzeppelin/guardian-operator-client';
import { AuthSecretKey, Word, getNativeModule } from '@miden-sdk/miden-sdk';

const GUARDIAN_URL = process.env.GUARDIAN_URL ?? 'http://127.0.0.1:3000';
const KEY_FILE = './operator-key.bin';

function bytesToHex(b: Uint8Array): string {
  return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

// Session cookies are not persisted automatically by Node.js fetch — wire them up manually.
function makeCookieFetch(): typeof fetch {
  const jar: Record<string, string> = {};
  return async (input, init) => {
    const headers = new Headers(init?.headers);
    const cookieHeader = Object.entries(jar)
      .map(([k, v]) => `${k}=${v}`)
      .join('; ');
    if (cookieHeader) headers.set('cookie', cookieHeader);

    const res = await fetch(input, { ...init, headers });

    // getSetCookie() returns each Set-Cookie header as a separate string (Node 18+).
    const setCookies =
      typeof res.headers.getSetCookie === 'function'
        ? res.headers.getSetCookie()
        : [res.headers.get('set-cookie') ?? ''].filter(Boolean);

    for (const raw of setCookies) {
      const [pair] = raw.split(';');
      const eq = pair?.indexOf('=') ?? -1;
      if (eq > 0) jar[pair.slice(0, eq).trim()] = pair.slice(eq + 1).trim();
    }

    return res;
  };
}


async function main() {
  if (!existsSync(KEY_FILE)) {
    // First run: generate a keypair, save the secret key to a file, exit.
    const secretKey = AuthSecretKey.rpoFalconWithRNG(undefined);
    writeFileSync(KEY_FILE, secretKey.serialize());
    // toHex() already includes the 0x prefix
    const commitment = secretKey.publicKey().toCommitment().toHex();
    const pubKeyHex = '0x' + bytesToHex(secretKey.publicKey().serialize().slice(1));

    const entry = JSON.stringify({ public_key: pubKeyHex, permissions: ['dashboard:read', 'accounts:pause'] }, null, 2);

    console.log(`\nNo key file found — generated a new Falcon keypair → ${KEY_FILE}\n`);
    console.log('1. Add this entry to docs/guides/miden-dashboard/operators.json:\n');
    console.log(entry.split('\n').map((l) => `   ${l}`).join('\n'));
    console.log(`\n   Commitment (for reference): ${commitment}\n`);
    console.log('2. Re-run (key loads automatically from operator-key.bin):\n');
    console.log('   npx tsx list-accounts.ts\n');
    return;
  }

  // The napi compat layer wrongly converts Buffer→Array for deserialize, so bypass it.
  const secretKey = getNativeModule().AuthSecretKey.deserialize(readFileSync(KEY_FILE)) as AuthSecretKey;
  // toHex() already includes the 0x prefix
  const commitment = secretKey.publicKey().toCommitment().toHex();

  const client = new GuardianOperatorHttpClient({
    baseUrl: GUARDIAN_URL,
    fetch: makeCookieFetch(),
  });

  // Step 1: request challenge
  process.stdout.write(`Authenticating  ${commitment.slice(0, 20)}…  `);
  const { challenge } = await client.challenge(commitment);

  // Step 2: sign the challenge digest with the Falcon key
  const sig = secretKey.sign(Word.fromHex(challenge.signingDigest));
  const sigHex = bytesToHex(sig.serialize().slice(1)); // drop leading scheme byte

  // Step 3: verify → server sets session cookie
  const { operatorId } = await client.verify({
    commitment,
    signature: sigHex,
  });
  console.log(`ok  (operator: ${operatorId})\n`);

  // List accounts
  const { items, nextCursor } = await client.listAccounts({ limit: 50 });
  const totalLabel = `${items.length}${nextCursor ? '+' : ''}`;

  console.log(`Accounts on ${GUARDIAN_URL}  (${totalLabel} total)`);
  console.log('─'.repeat(80));

  if (items.length === 0) {
    console.log('  No accounts registered yet.');
  } else {
    for (const a of items) {
      const status = a.pausedAt ? 'PAUSED' : 'active';
      console.log(
        `  [${status.padEnd(6)}]  ${a.accountId}` +
          `  scheme=${a.authScheme}` +
          `  signers=${a.authorizedSignerCount}`,
      );
      if (a.pausedReason) console.log(`             reason: ${a.pausedReason}`);
    }
  }
}

main().catch((e) => {
  console.error('\n' + (e instanceof Error ? e.message : String(e)));
  process.exit(1);
});
