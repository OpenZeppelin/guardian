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
  dashboardInfo: null,
  accountDeltas: null,
  accountProposals: null,
  globalDeltas: null,
  globalProposals: null,
  pagination: null,
  logout: null,
  postLogoutProtectedRequest: null,
};

function isPagedResult(value) {
  return (
    value &&
    Array.isArray(value.items) &&
    (value.nextCursor === null || typeof value.nextCursor === 'string')
  );
}

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
    (value) => Array.isArray(value?.items) && (value.nextCursor === null || typeof value.nextCursor === 'string'),
    'account list result',
  );
  result.accountList = {
    itemCount: accountList.items.length,
    nextCursor: accountList.nextCursor,
    firstAccountId: accountList.items[0]?.accountId ?? null,
  };

  const firstAccountId = accountList.items[0]?.accountId;
  if (firstAccountId) {
    await section(page, 'Accounts')
      .locator('label', { hasText: 'Account ID' })
      .locator('input')
      .fill(firstAccountId);
    await clickAction(page, 'Fetch account');
    const detail = await waitLastResult(
      page,
      (value) => value?.accountId === firstAccountId,
      'account detail result',
    );
    result.accountDetail = {
      accountId: detail.accountId,
      stateStatus: detail.stateStatus,
      authorizedSignerCount: detail.authorizedSignerCount,
    };
  } else {
    result.accountDetail = {
      skipped: true,
      reason: 'account list was empty',
    };
  }

  await clickAction(page, 'Dashboard info');
  const info = await waitLastResult(
    page,
    (value) =>
      (value?.serviceStatus === 'healthy' || value?.serviceStatus === 'degraded') &&
      typeof value.environment === 'string' &&
      typeof value.totalAccountCount === 'number' &&
      value.deltaStatusCounts &&
      typeof value.deltaStatusCounts.candidate === 'number' &&
      value.build &&
      typeof value.build.version === 'string' &&
      typeof value.build.gitCommit === 'string' &&
      (value.build.profile === 'debug' || value.build.profile === 'release') &&
      typeof value.build.startedAt === 'string' &&
      value.backend &&
      (value.backend.storage === 'filesystem' || value.backend.storage === 'postgres') &&
      Array.isArray(value.backend.supportedAckSchemes) &&
      (value.backend.canonicalization === null ||
        (value.backend.canonicalization &&
          typeof value.backend.canonicalization.checkIntervalSeconds === 'number')) &&
      value.accountsByAuthMethod &&
      typeof value.accountsByAuthMethod === 'object',
    'dashboard info result',
  );
  result.dashboardInfo = {
    serviceStatus: info.serviceStatus,
    environment: info.environment,
    build: info.build,
    backend: info.backend,
    totalAccountCount: info.totalAccountCount,
    accountsByAuthMethod: info.accountsByAuthMethod,
    inFlightProposalCount: info.inFlightProposalCount,
    deltaStatusCounts: info.deltaStatusCounts,
    degradedAggregates: info.degradedAggregates,
  };

  if (firstAccountId) {
    await clickAction(page, 'List account deltas');
    const deltas = await waitLastResult(
      page,
      isPagedResult,
      'account deltas result',
    );
    result.accountDeltas = {
      itemCount: deltas.items.length,
      nextCursor: deltas.nextCursor,
      firstNonce: deltas.items[0]?.nonce ?? null,
    };

    await clickAction(page, 'List account proposals');
    const proposals = await waitLastResult(
      page,
      isPagedResult,
      'account proposals result',
    );
    result.accountProposals = {
      itemCount: proposals.items.length,
      nextCursor: proposals.nextCursor,
      firstCommitment: proposals.items[0]?.commitment ?? null,
      firstProposalType: proposals.items[0]?.proposalType ?? null,
    };
  } else {
    result.accountDeltas = { skipped: true, reason: 'account list was empty' };
    result.accountProposals = { skipped: true, reason: 'account list was empty' };
  }

  await clickAction(page, 'List global deltas');
  const globalDeltas = await waitLastResult(
    page,
    isPagedResult,
    'global deltas result',
  );
  result.globalDeltas = {
    itemCount: globalDeltas.items.length,
    nextCursor: globalDeltas.nextCursor,
    firstHasAccountId:
      globalDeltas.items.length === 0 ||
      typeof globalDeltas.items[0]?.accountId === 'string',
  };

  await clickAction(page, 'List global proposals');
  const globalProposals = await waitLastResult(
    page,
    isPagedResult,
    'global proposals result',
  );
  result.globalProposals = {
    itemCount: globalProposals.items.length,
    nextCursor: globalProposals.nextCursor,
    firstHasAccountId:
      globalProposals.items.length === 0 ||
      typeof globalProposals.items[0]?.accountId === 'string',
    firstProposalType: globalProposals.items[0]?.proposalType ?? null,
  };

  await clickAction(page, 'Paginate accounts');
  const pagination = await waitLastResult(
    page,
    (value) =>
      isPagedResult(value?.firstPage) &&
      (value.secondPage === null || isPagedResult(value.secondPage)),
    'pagination result',
  );
  const firstPageId = pagination.firstPage.items[0]?.accountId ?? null;
  const secondPageId = pagination.secondPage?.items[0]?.accountId ?? null;
  result.pagination = {
    firstPageItemCount: pagination.firstPage.items.length,
    firstPageNextCursor: pagination.firstPage.nextCursor,
    firstPageFirstAccountId: firstPageId,
    secondPageFirstAccountId: secondPageId,
    cursorAdvanced:
      pagination.secondPage === null
        ? null
        : firstPageId !== null &&
          secondPageId !== null &&
          firstPageId !== secondPageId,
  };
  if (
    pagination.firstPage.items.length > 0 &&
    pagination.secondPage !== null &&
    result.pagination.cursorAdvanced !== true
  ) {
    throw new Error(
      `Pagination cursor did not advance: first=${firstPageId} second=${secondPageId}`,
    );
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
