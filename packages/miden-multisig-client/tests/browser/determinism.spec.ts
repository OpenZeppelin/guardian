import { expect, test } from '@playwright/test';

// Cross-SDK parity gate. The Rust upstream builder pins these values in
// `crates/contracts/src/multisig_guardian.rs::test_browser_deterministic_account_matches_rust_builder`.
const EXPECTED_ID = '0xade67f7701e9e9c12493c6206bc46e';
const EXPECTED_COMMITMENT =
  '0x0efd2d9b391c608de6814b57339894f448e3b2645609976b531bfa9c7ada3ca5';
// Storage commitment of the Rust account (7 slots, no schema-commitment slot). TS reproduces
// this exactly once it uses buildWithoutSchemaCommitment() — proving the storage layout matches.
const EXPECTED_STORAGE_COMMITMENT =
  '0xa5b24ee9ed2f2d73b8590851401bc20ed8bd0d588965a881e16ffecff8012c4f';

// Procedure roots that are threshold-override targets. These must be present in the TS-built
// account for cross-SDK threshold overrides to bind correctly.
const OVERRIDE_TARGET_PROCEDURES = [
  'update_signers',
  'update_procedure_threshold',
  'update_guardian',
  'send_asset',
  'receive_asset',
];

async function buildInBrowser(page: import('@playwright/test').Page) {
  const consoleErrors: string[] = [];
  page.on('console', (m) => {
    if (m.type() === 'error') consoleErrors.push(m.text());
  });
  page.on('pageerror', (e) => consoleErrors.push(String(e)));
  await page.goto('/tests/browser/harness.html');
  await page.waitForFunction(() => Boolean(window.__result || window.__error), null, {
    timeout: 170_000,
  });
  const harnessError = await page.evaluate(() => window.__error);
  expect(harnessError, `harness threw:\n${harnessError}\n${consoleErrors.join('\n')}`).toBeFalsy();
  return page.evaluate(() => window.__result);
}

test('TS account reproduces the Rust storage layout and override-target procedures', async ({
  page,
}) => {
  const result = await buildInBrowser(page);
  console.log('TS account decomposition:', JSON.stringify(result, null, 2));

  // Storage layout parity (slot names, order, values) — holds across SDKs.
  expect(result?.storageCommitment).toBe(EXPECTED_STORAGE_COMMITMENT);
  expect(result?.slotNames).toHaveLength(7);

  // Every threshold-override-target procedure root resolves in the TS-built account, so
  // per-procedure overrides set via the SDK bind to real procedures.
  const hasProcedure = result?.hasProcedure as Record<string, boolean> | undefined;
  for (const name of OVERRIDE_TARGET_PROCEDURES) {
    expect(hasProcedure?.[name], `missing override-target procedure: ${name}`).toBe(true);
  }

  // The config transaction scripts (`@transaction_script pub proc main`, mirroring the Rust
  // builders) compile against the SDK's 0.16 assembler; a syntax or module-path drift between
  // the SDKs fails here instead of at a cosigner's first config proposal.
  const configScriptsCompiled = result?.configScriptsCompiled as
    | Record<string, boolean>
    | undefined;
  for (const name of ['updateSigners', 'updateProcedureThreshold', 'updateGuardian']) {
    expect(configScriptsCompiled?.[name], `config script failed to compile: ${name}`).toBe(true);
  }
});

// `@miden-sdk/miden-sdk` 0.16.0-rc.2 bundles the upstream `miden-standards` guarded-multisig
// component matching the Rust pin (`miden-standards = "=0.16.0-rc.4"`), so
// `auth_tx_guarded_multisig` compiles to the same MAST and a TS-built account is byte-identical to
// the Rust/server account.
test(
  'TS account id + commitment match the Rust builder',
  async ({ page }) => {
    const result = await buildInBrowser(page);
    expect(result?.id).toBe(EXPECTED_ID);
    expect(result?.commitment).toBe(EXPECTED_COMMITMENT);
  },
);
