/**
 * Generate a Falcon operator keypair for the Guardian dashboard / allowlist.
 * Adapted from 0xMiden/guardian-dashboard's scripts/generate-operator-key.ts to
 * run standalone here (the Miden SDK is already a dependency of this scripts
 * package — see ./package.json).
 *
 * Run:
 *   npm install
 *   npx tsx generate-operator-key.ts
 *
 * Outputs the three values you need:
 *   - private key + commitment → the dashboard's GUARDIAN_ENDPOINTS entry
 *     (privateKey / commitment), or GUARDIAN_OPERATOR_PRIVATE_KEY for operator-demo.ts
 *   - public key → an entry in operators.json (the Guardian allowlist)
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

type SDK = typeof import('@miden-sdk/miden-sdk') & {
  initSync: (opts: { module: BufferSource | WebAssembly.Module }) => void;
};

async function main() {
  const mod = (await import('@miden-sdk/miden-sdk/lazy')) as SDK;
  // Resolve the .wasm by path relative to THIS script (not process.cwd), so it
  // works no matter where you invoke it from. The package's `exports` map does
  // not expose this asset to require.resolve, hence the direct path.
  const wasmPath = fileURLToPath(
    new URL(
      './node_modules/@miden-sdk/miden-sdk/dist/st/assets/miden_client_web.wasm',
      import.meta.url,
    ),
  );
  mod.initSync({ module: readFileSync(wasmPath) });

  const bytesToHex = (bytes: Uint8Array): string =>
    Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');

  const secretKey = mod.AuthSecretKey.rpoFalconWithRNG();
  const publicKey = secretKey.publicKey();

  const privateKeyHex = bytesToHex(secretKey.serialize());
  const publicKeyHex = '0x' + bytesToHex(publicKey.serialize().slice(1));
  const commitment = publicKey.toCommitment().toHex();

  console.log('# ── For the dashboard .env (GUARDIAN_ENDPOINTS entry) ─────────');
  console.log(`#    privateKey: ${privateKeyHex}`);
  console.log(`#    commitment: ${commitment}`);
  console.log('');
  console.log('# ── For operator-demo.ts ──────────────────────────────────────');
  console.log(`GUARDIAN_OPERATOR_PRIVATE_KEY=${privateKeyHex}`);
  console.log('');
  console.log('# ── Add to operators.json (the Guardian allowlist) ───────────');
  console.log(
    JSON.stringify(
      [{ public_key: publicKeyHex, permissions: ['dashboard:read', 'accounts:pause'] }],
      null,
      2,
    ),
  );
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
