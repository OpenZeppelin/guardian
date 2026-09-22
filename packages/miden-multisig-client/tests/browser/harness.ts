// Uses the Rust parity test's fixed inputs and exposes results to Playwright.
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
  // A mock chain: account construction and script compilation need no node, and a
  // node on another protocol line would reject the client before serving anything.
  const client = await MidenClient.createMock();

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
    'mock',
  );
  const accountId = account.id().toString();

  const code = account.code();
  const hasProcedure: Record<string, boolean> = {};
  for (const [name, root] of Object.entries(PROCEDURE_ROOTS)) {
    hasProcedure[name] = code.hasProcedure(Word.fromHex(root));
  }

  // Compile every config script against the real WASM assembler, and have the
  // client attach the account's multisig auth args to each request.
  const requestOptions = { accountId, midenRpcEndpoint: 'mock' };
  const configScriptsCompiled: Record<string, boolean> = {};
  const authArgsAttached: Record<string, boolean> = {};
  const signers = await buildUpdateSignersTransactionRequest(client, 1, [SIGNER_COMMITMENT], requestOptions);
  configScriptsCompiled.updateSigners = true;
  authArgsAttached.updateSigners = Boolean(signers.request.authArg());
  const threshold = await buildUpdateProcedureThresholdTransactionRequest(client, 'send_asset', 2, requestOptions);
  configScriptsCompiled.updateProcedureThreshold = true;
  authArgsAttached.updateProcedureThreshold = Boolean(threshold.request.authArg());
  const guardian = await buildUpdateGuardianTransactionRequest(client, GUARDIAN_COMMITMENT, requestOptions);
  configScriptsCompiled.updateGuardian = true;
  authArgsAttached.updateGuardian = Boolean(guardian.request.authArg());

  window.__result = {
    id: account.id().toString(),
    commitment: account.to_commitment().toHex(),
    codeCommitment: account.code().commitment().toHex(),
    storageCommitment: account.storage().commitment().toHex(),
    slotNames: account.storage().getSlotNames(),
    hasProcedure,
    configScriptsCompiled,
    authArgsAttached,
  };
  report(JSON.stringify(window.__result, null, 2));
}

run().catch((error: unknown) => {
  const err = error as { stack?: string };
  window.__error = String((err && err.stack) || error);
  report(`ERROR: ${window.__error}`);
});
