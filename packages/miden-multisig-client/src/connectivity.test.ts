import { describe, it, expect } from 'vitest';
import { GuardianHttpError } from '@openzeppelin/guardian-client';
import { isLikelyNetworkError, toUserFacingError } from './connectivity.js';

describe('isLikelyNetworkError', () => {
  it('flags codeless transport failures', () => {
    for (const m of [
      'Failed to fetch',
      'NetworkError when attempting to fetch resource',
      'Load failed',
      'The operation was aborted',
      'request timed out',
      'connection refused',
      'getaddrinfo ENOTFOUND guardian.example',
    ]) {
      expect(isLikelyNetworkError(new TypeError(m))).toBe(true);
    }
  });

  it('does not flag semantic errors', () => {
    expect(isLikelyNetworkError(new Error('account is paused'))).toBe(false);
    expect(isLikelyNetworkError(new Error('insufficient signatures'))).toBe(false);
  });
});

describe('toUserFacingError', () => {
  it('uses the server code + user-safe message when Guardian was reached', () => {
    const body = JSON.stringify({
      code: 'account_paused',
      message: "This account is paused and can't approve transactions right now.",
      meta: { retryable: false },
    });
    const result = toUserFacingError(new GuardianHttpError(409, 'Conflict', body));
    expect(result.code).toBe('account_paused');
    expect(result.userMessage).toContain('paused');
    expect(result.category).toBeUndefined();
  });

  it('classifies a codeless transport failure as connectivity', () => {
    const result = toUserFacingError(new TypeError('Failed to fetch'));
    expect(result.code).toBeUndefined();
    expect(result.category).toBe('unreachable');
    expect(result.userMessage).toContain("Can't reach Guardian");
    // The raw transport text is never the primary message.
    expect(result.userMessage).not.toContain('Failed to fetch');
  });

  it('classifies timeouts and aborts as the timeout category', () => {
    for (const m of ['request timed out', 'The operation was aborted']) {
      const result = toUserFacingError(new Error(m));
      expect(result.category).toBe('timeout');
      expect(result.userMessage).toContain("Can't reach Guardian");
    }
  });

  it('treats a reachable proxy 5xx with no Guardian body as connectivity', () => {
    const result = toUserFacingError(new GuardianHttpError(502, 'Bad Gateway', '<html>nope</html>'));
    expect(result.category).toBe('unreachable');
    expect(result.userMessage).toContain("Can't reach Guardian");
  });

  it('falls back to a generic message for unknown non-Guardian errors', () => {
    const result = toUserFacingError(new Error('totally unexpected'));
    expect(result.userMessage).toBe('Something went wrong. Please try again.');
    expect(result.userMessage).not.toContain('totally unexpected');
  });
});
