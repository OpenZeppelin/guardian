// Browser harness: builds a guarded-multisig account with the SAME fixed inputs as the Rust
// cross-SDK parity test (`crates/contracts/src/multisig_guardian.rs`
// `test_browser_deterministic_account_matches_rust_builder`) and exposes the resulting
// account id + commitment on `window` for Playwright to read. Runs against the built `dist/`
// artifact and the real WASM SDK in a browser (the node/vitest harness cannot — its store
// needs IndexedDB). No network: `autoSync` is off and only local compile + store writes run.
import { MidenClient, Word } from '@miden-sdk/miden-sdk';

import {
  buildUpdateGuardianTransactionRequest,
  buildUpdateProcedureThresholdTransactionRequest,
  buildUpdateSignersTransactionRequest,
  createMultisigAccount,
} from '../../dist/index.js';
import { PROCEDURE_ROOTS } from '../../dist/procedures.js';

const SIGNER_COMMITMENT =
  '0x260a375ca01f1f05cd7bf22298b40c47290fc09f209011d39049b7f2ef61387b';
const GUARDIAN_COMMITMENT =
  '0xc35d79423c41d46b5289aafef48be2364e9ea494c6b14d6aefad10f1a46e6d7c';

declare global {
  interface Window {
    __result?: { id: string; commitment: string; [key: string]: unknown };
    __error?: string;
  }
}

function report(message: string): void {
  const out = document.getElementById('out');
  if (out) out.textContent = message;
}

async function run(): Promise<void> {
  const client = await MidenClient.create({
    rpcUrl: 'https://rpc.devnet.miden.io',
    storeName: `determinism-${Math.random().toString(36).slice(2)}`,
    autoSync: false,
  });

  const seed = new Uint8Array(32);
  seed.fill(9);

  const { account } = await createMultisigAccount(
    client as never,
    {
      threshold: 1,
      signerCommitments: [SIGNER_COMMITMENT],
      guardianCommitment: GUARDIAN_COMMITMENT,
      seed,
    },
    'https://rpc.devnet.miden.io',
  );

  const code = account.code();
  const hasProcedure: Record<string, boolean> = {};
  for (const [name, root] of Object.entries(PROCEDURE_ROOTS)) {
    hasProcedure[name] = code.hasProcedure(Word.fromHex(root));
  }

  // Compile every config transaction script against the real 0.16 WASM assembler. These
  // scripts must mirror the Rust builders (`@transaction_script pub proc main`); a syntax or
  // module-path drift fails compilation here instead of at a cosigner's first config proposal.
  const rpcOptions = { midenRpcEndpoint: 'https://rpc.devnet.miden.io' };
  const configScriptsCompiled: Record<string, boolean> = {};
  await buildUpdateSignersTransactionRequest(client, 1, [SIGNER_COMMITMENT], rpcOptions);
  configScriptsCompiled.updateSigners = true;
  await buildUpdateProcedureThresholdTransactionRequest(client, 'send_asset', 2, rpcOptions);
  configScriptsCompiled.updateProcedureThreshold = true;
  await buildUpdateGuardianTransactionRequest(client, GUARDIAN_COMMITMENT, rpcOptions);
  configScriptsCompiled.updateGuardian = true;

  window.__result = {
    id: account.id().toString(),
    commitment: account.to_commitment().toHex(),
    codeCommitment: account.code().commitment().toHex(),
    storageCommitment: account.storage().commitment().toHex(),
    slotNames: account.storage().getSlotNames(),
    hasProcedure,
    configScriptsCompiled,
  };
  report(JSON.stringify(window.__result, null, 2));
}

run().catch((error: unknown) => {
  const err = error as { stack?: string };
  window.__error = String((err && err.stack) || error);
  report(`ERROR: ${window.__error}`);
});
