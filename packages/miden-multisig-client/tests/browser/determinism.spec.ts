import { expect, test } from '@playwright/test';

// Cross-SDK parity gate. The Rust upstream builder pins these values in
// `crates/contracts/src/multisig_guardian.rs::test_browser_deterministic_account_matches_rust_builder`.
const EXPECTED_ID = '0xf9bf6e86166a2101217ff39e1ddfa2';
const EXPECTED_COMMITMENT =
  '0x25d8ea5d0525be44cd23052359893d8242b2bd6c643c9f35b9096de55bcace55';
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
});

// KNOWN BLOCKER — dependency version skew. The npm `@miden-sdk/miden-sdk` (0.15.0) bundles a
// different miden-standards patch than the Rust pin (0.15.3), so the auth-flow internals
// (`auth_tx_guarded_multisig`) compile to a different MAST. This makes the full account
// code-commitment/id diverge across SDKs even though storage + override-target procedures match.
// A TS-created account is therefore NOT byte-identical to the Rust/server account until the web
// SDK and Rust crates are aligned to the same standards patch. Unskip once aligned.
test.fixme(
  'TS account id + commitment match the Rust builder (blocked on web SDK vs Rust standards version alignment)',
  async ({ page }) => {
    const result = await buildInBrowser(page);
    expect(result?.id).toBe(EXPECTED_ID);
    expect(result?.commitment).toBe(EXPECTED_COMMITMENT);
  },
);
