import { expect, test } from '@playwright/test';

// Pinned by the Rust SDK's `guardian_executable_vectors_the_typescript_sdk_must_reproduce`
// (`crates/miden-multisig-client/src/transaction/expiration.rs`). Both SDKs must derive the same
// Guardian-executable auth arguments and the same scripts with the transaction expiration, so
// the same effects yield the same summary and proposal id in either. Regenerate both sides
// when the Miden pins change.
const EXPECTED = {
  authArg: '0xec66ad44aba543365dc7d859d9e4b09cbd84bfdfb76fbb388b994eeafc0546b8',
  p2idRecipient: '0x841f13d50f06ba63ed713dbb9e5b3a6988ac7bf2895281e6974a6c3f86c1aa5d',
  updateSigners: '0xef1d061e74fa7da827343f1f387f45e93286de76c97495ea59a643d22291be9c',
  updateProcedureThreshold: '0x754d45172e729f2da08f1690152115b7f8eb3272441cb1a77c1614afeed4c5db',
  updateGuardian: '0xf8c248ad4859a455a1ba0ad26501821fe1555e719972e0ad7eb487c983319d09',
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
