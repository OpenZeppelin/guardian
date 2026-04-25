import path from 'node:path';
import { mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';

const smokeUrl = process.env.GUARDIAN_OPERATOR_SMOKE_URL ?? 'http://127.0.0.1:3003/';
const guardianUrl = process.env.GUARDIAN_URL ?? 'http://127.0.0.1:3000';
const playwrightInstallRoot =
  process.env.PLAYWRIGHT_CORE_INSTALL_ROOT ??
  '/tmp/guardian-operator-smoke-playwright';
const chromeExecutable =
  process.env.CHROME_EXECUTABLE ??
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const headless = process.env.HEADLESS !== 'false';
const operatorPublicKeysFile =
  process.env.GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE ??
  '/tmp/guardian-operator-smoke/operator-public-keys.json';

const require = createRequire(path.join(playwrightInstallRoot, 'package.json'));
const { chromium } = require('playwright-core');

function section(page, heading) {
  return page.locator('section.panel', {
    has: page.getByRole('heading', { name: heading }),
  });
}

async function sectionPreJson(page, heading) {
  const text = await section(page, heading).locator('pre').first().innerText({ timeout: 20_000 });
  return JSON.parse(text);
}

async function waitLastResult(page, predicate, label) {
  const started = Date.now();
  while (Date.now() - started < 30_000) {
    const value = await sectionPreJson(page, 'Last Result').catch(() => null);
    if (value && predicate(value)) return value;
    await page.waitForTimeout(100);
  }
  throw new Error(`Timed out waiting for ${label}`);
}

async function waitForNoBusy(page) {
  const busy = page.locator('text=Busy:');
  if ((await busy.count()) === 0) return;
  await busy.first().waitFor({ state: 'detached', timeout: 60_000 });
}

async function clickAction(page, name) {
  await page.getByRole('button', { name }).click();
  await waitForNoBusy(page);
}

async function localSignerReady(page) {
  return (
    (await section(page, 'Signer').locator('.badge').filter({ hasText: 'Ready' }).count()) > 0
  );
}

async function uiError(page) {
  const error = page.locator('.error-box').filter({ hasText: 'Action error:' }).last();
  if ((await error.count()) === 0) return null;
  return error.innerText();
}

const browser = await chromium.launch({
  executablePath: chromeExecutable,
  headless,
});

const result = {
  guardianUrl,
  smokeUrl,
  operatorClientSource: 'workspace file:../../packages/guardian-operator-client',
  operatorPublicKeysFile,
  operatorPublicKeysJson: null,
  publicKey: null,
  commitment: null,
  challenge: null,
  login: null,
  accountList: null,
  accountDetail: null,
  logout: null,
  postLogoutProtectedRequest: null,
};

try {
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(60_000);
  await page.goto(smokeUrl, { waitUntil: 'networkidle' });

  if (!(await localSignerReady(page))) {
    await clickAction(page, 'Generate local Falcon signer');
  }
  const operatorPublicKeysJson = await section(page, 'Operator Public Keys JSON')
    .locator('pre')
    .first()
    .innerText({ timeout: 20_000 });
  const operatorPublicKeys = JSON.parse(operatorPublicKeysJson);
  if (!Array.isArray(operatorPublicKeys)) {
    throw new Error('Operator public keys JSON must be an array');
  }
  const publicKey = typeof operatorPublicKeys[0] === 'string' ? operatorPublicKeys[0].trim() : '';
  if (!publicKey) {
    throw new Error('Operator public keys JSON did not contain a public key');
  }
  await mkdir(path.dirname(operatorPublicKeysFile), { recursive: true });
  await writeFile(operatorPublicKeysFile, `${JSON.stringify(operatorPublicKeys, null, 2)}\n`);
  const commitment = await section(page, 'Session')
    .locator('label', { hasText: 'Operator commitment' })
    .locator('input')
    .inputValue();
  if (!commitment) {
    throw new Error('Operator commitment was empty');
  }
  result.operatorPublicKeysJson = operatorPublicKeysJson;
  result.publicKey = publicKey;
  result.commitment = commitment;

  const challengeResponse = await fetch(
    `${guardianUrl}/auth/challenge?commitment=${encodeURIComponent(commitment)}`,
  );
  result.challenge = await challengeResponse.json();
  if (!challengeResponse.ok || result.challenge?.success !== true) {
    throw new Error(`Challenge request failed: ${JSON.stringify(result.challenge)}`);
  }

  await clickAction(page, 'Login');
  result.login = await waitLastResult(
    page,
    (value) => value?.success === true && typeof value.operatorId === 'string',
    'login result',
  );

  await clickAction(page, 'List accounts');
  const accountList = await waitLastResult(
    page,
    (value) => value?.success === true && Array.isArray(value.accounts),
    'account list result',
  );
  result.accountList = {
    success: accountList.success,
    totalCount: accountList.totalCount,
    firstAccountId: accountList.accounts[0]?.accountId ?? null,
  };

  const firstAccountId = accountList.accounts[0]?.accountId;
  if (firstAccountId) {
    await section(page, 'Accounts')
      .locator('label', { hasText: 'Account ID' })
      .locator('input')
      .fill(firstAccountId);
    await clickAction(page, 'Fetch account');
    const detail = await waitLastResult(
      page,
      (value) => value?.success === true && value.account?.accountId === firstAccountId,
      'account detail result',
    );
    result.accountDetail = {
      success: detail.success,
      accountId: detail.account.accountId,
      stateStatus: detail.account.stateStatus,
      authorizedSignerCount: detail.account.authorizedSignerCount,
    };
  } else {
    result.accountDetail = {
      skipped: true,
      reason: 'account list was empty',
    };
  }

  await clickAction(page, 'Logout');
  result.logout = await waitLastResult(page, (value) => value?.success === true, 'logout result');

  await clickAction(page, 'List accounts');
  result.postLogoutProtectedRequest = await uiError(page);
  if (!result.postLogoutProtectedRequest) {
    throw new Error('Expected a protected request error after logout');
  }

  console.log(JSON.stringify(result, null, 2));
} finally {
  await browser.close();
}
