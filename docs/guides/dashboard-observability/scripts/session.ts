/**
 * Shared operator session helper. `connect()` hides all the Node-specific
 * boilerplate — Miden SDK (WASM) init, loading the Falcon key, a cookie jar so
 * the session survives across fetch calls, and the challenge → sign → verify
 * login — and returns an authenticated client. Every operation script imports
 * this so each stays a few readable lines.
 *
 * Env:
 *   GUARDIAN_URL                  default http://localhost:3000
 *   GUARDIAN_OPERATOR_PRIVATE_KEY hex private key from generate-operator-key.ts (required)
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { GuardianOperatorHttpClient } from '@openzeppelin/guardian-operator-client';

export const GUARDIAN_URL = process.env.GUARDIAN_URL ?? 'http://localhost:3000';

const hexToBytes = (hex: string): Uint8Array => {
  const clean = hex.startsWith('0x') ? hex.slice(2) : hex;
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
};

const bytesToHex = (bytes: Uint8Array): string =>
  '0x' + Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');

// A minimal cookie jar so the operator session cookie is replayed on every
// subsequent call. Browser `fetch` does this automatically; Node `fetch` does not.
function makeCookieFetch(base: typeof fetch = globalThis.fetch): typeof fetch {
  const jar = new Map<string, string>();
  return (async (input: RequestInfo | URL, init: RequestInit = {}) => {
    const headers = new Headers(init.headers);
    if (jar.size > 0) {
      headers.set('cookie', [...jar].map(([k, v]) => `${k}=${v}`).join('; '));
    }
    const res = await base(input, { ...init, headers });
    const h = res.headers as Headers & { getSetCookie?: () => string[] };
    const setCookies =
      typeof h.getSetCookie === 'function'
        ? h.getSetCookie()
        : res.headers.get('set-cookie')
          ? [res.headers.get('set-cookie') as string]
          : [];
    for (const sc of setCookies) {
      const pair = sc.split(';', 1)[0];
      const eq = pair.indexOf('=');
      if (eq > 0) jar.set(pair.slice(0, eq).trim(), pair.slice(eq + 1).trim());
    }
    return res;
  }) as typeof fetch;
}

export interface OperatorSession {
  client: GuardianOperatorHttpClient;
  commitment: string;
}

/** Initialize the SDK, load the operator key, and log in. Returns an
 *  authenticated client plus the operator's commitment. */
export async function connect(): Promise<OperatorSession> {
  const privateKeyHex = process.env.GUARDIAN_OPERATOR_PRIVATE_KEY;
  if (!privateKeyHex) {
    console.error(
      'Set GUARDIAN_OPERATOR_PRIVATE_KEY to the hex private key from generate-operator-key.ts.',
    );
    process.exit(1);
  }

  // The bare `@miden-sdk/miden-sdk` import resolves to a NAPI build whose
  // AuthSecretKey.deserialize throws in Node; the `/lazy` WASM build works. The
  // package `exports` map doesn't expose the .wasm, so resolve it by path.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const sdk: any = await import('@miden-sdk/miden-sdk/lazy');
  const wasmPath = fileURLToPath(
    new URL(
      './node_modules/@miden-sdk/miden-sdk/dist/st/assets/miden_client_web.wasm',
      import.meta.url,
    ),
  );
  sdk.initSync({ module: readFileSync(wasmPath) });
  const { AuthSecretKey, Word } = sdk;

  const secretKey = AuthSecretKey.deserialize(hexToBytes(privateKeyHex));
  const commitment: string = secretKey.publicKey().toCommitment().toHex();

  const client = new GuardianOperatorHttpClient({
    baseUrl: GUARDIAN_URL,
    fetch: makeCookieFetch(),
  });

  const { challenge } = await client.challenge(commitment);
  const signature = secretKey.sign(Word.fromHex(challenge.signingDigest));
  await client.verify({
    commitment,
    signature: bytesToHex(signature.serialize().slice(1)),
  });

  return { client, commitment };
}
