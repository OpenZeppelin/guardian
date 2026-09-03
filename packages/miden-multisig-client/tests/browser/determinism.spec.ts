import { expect, test } from '@playwright/test';

// Pinned by the corresponding Rust cross-SDK parity test.
const EXPECTED_ID = '0xbe7041e78ee3f1410aa5b790fa9c11';
const EXPECTED_COMMITMENT =
  '0xa7c22bff9e22a6463124355881a7f24b9862bfebb41ae2b11f540aaf0ddb10bb';
// Rust account storage commitment: seven slots without a schema-commitment slot.
const EXPECTED_STORAGE_COMMITMENT =
  '0xa5b24ee9ed2f2d73b8590851401bc20ed8bd0d588965a881e16ffecff8012c4f';

// Threshold overrides must resolve to procedures in the TypeScript-built account.
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

  // Config scripts must compile against the SDK's real WASM assembler.
  const configScriptsCompiled = result?.configScriptsCompiled as
    | Record<string, boolean>
    | undefined;
  for (const name of ['updateSigners', 'updateProcedureThreshold', 'updateGuardian']) {
    expect(configScriptsCompiled?.[name], `config script failed to compile: ${name}`).toBe(true);
  }
});

// TS-built account id and commitment must match the Rust builder
// (`test_browser_deterministic_account_matches_rust_builder`).
test(
  'TS account id + commitment match the Rust builder',
  async ({ page }) => {
    const result = await buildInBrowser(page);
    expect(result?.id).toBe(EXPECTED_ID);
    expect(result?.commitment).toBe(EXPECTED_COMMITMENT);
  },
);
