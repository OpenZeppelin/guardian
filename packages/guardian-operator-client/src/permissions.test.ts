import { describe, expect, it } from 'vitest';

import {
  ACCOUNTS_PAUSE,
  DASHBOARD_READ,
  POLICIES_WRITE,
} from './permissions.js';

describe('operator permission wire strings', () => {
  it('exposes the v1 permission consts as the expected wire strings', () => {
    // Renaming any of these is a breaking contract change against the
    // server's `Permission::as_str` vocabulary
    // (crates/server/src/dashboard/permissions.rs).
    expect(DASHBOARD_READ).toBe('dashboard:read');
    expect(ACCOUNTS_PAUSE).toBe('accounts:pause');
    expect(POLICIES_WRITE).toBe('policies:write');
  });
});
