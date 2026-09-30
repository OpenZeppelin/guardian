import type { ActionOutcome, ActionContext } from '../runner.js';

/**
 * Syntactically valid and deliberately never registered. The account lookup
 * runs before timestamp validation and before signature verification, so a
 * throwaway credential still reaches the envelope this asserts.
 */
const UNREGISTERED_ACCOUNT_ID = '0xaabbccddeeff00011b27a8df4ddbe0';
const EXPECTED_CODE = 'account_not_found';

interface ErrorBody {
  code: string;
  message: string;
  meta: { retryable: boolean };
}

export async function assertHttpEnvelope(context: ActionContext): Promise<ActionOutcome> {
  const url = `${context.httpEndpoint}/state?account_id=${UNREGISTERED_ACCOUNT_ID}`;
  let response: Response;
  try {
    response = await fetch(url, {
      headers: {
        'x-pubkey': '00'.repeat(32),
        'x-signature': '00'.repeat(64),
        'x-timestamp': String(Math.floor(Date.now() / 1000)),
      },
    });
  } catch (error) {
    return { kind: 'failed', classification: 'setup', reason: `GET ${url} failed: ${String(error)}` };
  }

  let body: ErrorBody;
  try {
    body = (await response.json()) as ErrorBody;
  } catch (error) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `error response carried no structured envelope: ${String(error)}`,
    };
  }

  if (response.status !== 404) {
    return {
      kind: 'failed',
      classification: 'product',
      reason: `expected 404 for an unregistered account, got ${response.status} with code ${body.code}`,
    };
  }
  if (body.code !== EXPECTED_CODE) {
    return { kind: 'failed', classification: 'product', reason: `expected code ${EXPECTED_CODE}, got ${body.code}` };
  }
  if (!body.message?.trim()) {
    return { kind: 'failed', classification: 'product', reason: 'envelope carried an empty message' };
  }
  if (body.meta?.retryable) {
    return { kind: 'failed', classification: 'product', reason: 'an unregistered account was reported as retryable' };
  }
  return { kind: 'passed' };
}
