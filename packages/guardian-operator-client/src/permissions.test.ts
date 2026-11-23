import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  ACCOUNTS_PAUSE,
  DASHBOARD_READ,
  MODULES_DISABLE,
  POLICIES_WRITE,
  POLICIES_WRITE_GLOBAL,
  STATS_REFRESH,
} from './permissions.js';

const CLIENT_PERMISSIONS = [
  DASHBOARD_READ,
  ACCOUNTS_PAUSE,
  POLICIES_WRITE,
  POLICIES_WRITE_GLOBAL,
  MODULES_DISABLE,
  STATS_REFRESH,
];

describe('operator permission wire strings', () => {
  it('exposes the v1 permission consts as the expected wire strings', () => {
    // Renaming any of these is a breaking contract change against the
    // server's `Permission::as_str` vocabulary
    // (crates/server/src/dashboard/permissions.rs).
    expect(DASHBOARD_READ).toBe('dashboard:read');
    expect(ACCOUNTS_PAUSE).toBe('accounts:pause');
    expect(POLICIES_WRITE).toBe('policies:write');
    expect(POLICIES_WRITE_GLOBAL).toBe('policies:write_global');
    expect(MODULES_DISABLE).toBe('modules:disable');
    expect(STATS_REFRESH).toBe('stats:refresh');
  });

  it('covers every permission the server defines', () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const permissionsRs = readFileSync(
      resolve(here, '../../../crates/server/src/dashboard/permissions.rs'),
      'utf8',
    );
    const serverPermissions = [
      ...permissionsRs.matchAll(/^pub const [A-Z_]+: &str = "([^"]+)";$/gm),
    ].map((match) => match[1]);
    expect(serverPermissions.length).toBeGreaterThan(0);
    expect([...serverPermissions].sort()).toEqual([...CLIENT_PERMISSIONS].sort());
  });
});
