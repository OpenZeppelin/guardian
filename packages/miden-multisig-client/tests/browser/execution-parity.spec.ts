import { expect, test } from '@playwright/test';

// Pinned by the Rust SDK's `guardian_executable_vectors_the_typescript_sdk_must_reproduce`
// (`crates/miden-multisig-client/src/transaction/expiration.rs`). Both SDKs must derive the same
// Guardian-executable auth arguments and the same scripts with the transaction expiration, so
// the same effects yield the same summary and proposal id in either. Regenerate both sides
// when the Miden pins change.
const EXPECTED = {
  authArg: '0xec66ad44aba543365dc7d859d9e4b09cbd84bfdfb76fbb388b994eeafc0546b8',
  updateSigners: '0x10812c997e2896a5be3684de226024527df5fcdb6d6a9e2ba76032e5ca9a2c56',
  updateProcedureThreshold: '0x62ca1dd0ed90a2747a28abc917068dcb0b58dcb1f355460242e35fe2227f72ca',
  updateGuardian: '0x9aadf7d81015fe48295b290aef188da918b99c748b5db1a632081f85f6e446e5',
};

test('Guardian-executable auth arguments and scripts match the Rust SDK', async ({ page }) => {
  await page.goto('/tests/browser/harness.html');
  await page.waitForFunction(() => Boolean(window.__result || window.__error), null, {
    timeout: 170_000,
  });
  const harnessError = await page.evaluate(() => window.__error);
  expect(harnessError, `harness threw:\n${harnessError}`).toBeFalsy();
  const vectors = await page.evaluate(() => window.__result?.guardianExecutableVectors);
  expect(vectors).toEqual(EXPECTED);
});
