import { expect, test } from '@playwright/test';

// Cross-SDK parity gate. The Rust upstream builder pins these values in
// `crates/contracts/src/multisig_guardian.rs::test_browser_deterministic_account_matches_rust_builder`.
const EXPECTED_ID = '0x8fc3d82cee89e3614b5e3e215db370';
const EXPECTED_COMMITMENT =
  '0x9fa18826a999fa5ac79c615a00905b3e09e5e0a703a65f167d1c836e51e8e08e';
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

// `@miden-sdk/miden-sdk` 0.16.0-alpha.1 bundles the upstream `miden-standards` guarded-multisig
// component matching the Rust pin (`miden-standards = "=0.16.0-alpha.4"`), so
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
