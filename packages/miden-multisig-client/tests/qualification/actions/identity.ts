import type { ActionOutcome, ActionContext } from '../runner.js';

interface StatusResponse {
  status: string;
  version: string;
  git_commit: string;
  environment: string;
}

export async function assertIdentity(context: ActionContext): Promise<ActionOutcome> {
  const url = `${context.httpEndpoint}/status`;
  let response: Response;
  try {
    response = await fetch(url);
  } catch (error) {
    return { kind: 'failed', classification: 'setup', reason: `GET ${url} failed: ${String(error)}` };
  }

  if (!response.ok) {
    return { kind: 'failed', classification: 'product', reason: `GET ${url} returned ${response.status}` };
  }

  const status = (await response.json()) as StatusResponse;

  if (status.status !== 'ok') {
    return { kind: 'failed', classification: 'product', reason: `server reports status ${status.status}` };
  }
  if (status.git_commit === 'unknown') {
    return {
      kind: 'failed',
      classification: 'setup',
      reason:
        'server reports an unknown commit; the image was built without its source revision, so the identity assertion would compare nothing',
    };
  }
  const expected = context.imageRevision;
  if (expected && !expected.startsWith(status.git_commit) && !status.git_commit.startsWith(expected)) {
    return {
      kind: 'failed',
      classification: 'setup',
      reason: `server reports commit ${status.git_commit} but the artifact under test is ${expected}`,
    };
  }
  if (!status.version || !status.environment) {
    return { kind: 'failed', classification: 'product', reason: 'server reported an empty version or environment' };
  }
  return { kind: 'passed' };
}
