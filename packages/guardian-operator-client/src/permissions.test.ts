import { describe, expect, it } from 'vitest';

import {
  ACCOUNTS_PAUSE,
  DASHBOARD_READ,
  POLICIES_WRITE,
  REQUIRED_PERMISSIONS,
  requiredPermissionsFor,
} from './permissions.js';

describe('REQUIRED_PERMISSIONS (feature 006-operator-authz)', () => {
  it('matches the snapshot of per-endpoint required permission sets', () => {
    // Any change to this snapshot must be paired with the corresponding
    // server-side route requirement in `crates/server/src/builder/handle.rs`.
    // The snapshot is sorted alphabetically by endpoint key so adding a
    // method surfaces as a single-line diff.
    expect(REQUIRED_PERMISSIONS).toMatchInlineSnapshot(`
      {
        "getAccount": [
          "dashboard:read",
        ],
        "getAccountSnapshot": [
          "dashboard:read",
        ],
        "getDashboardInfo": [
          "dashboard:read",
        ],
        "listAccountDeltas": [
          "dashboard:read",
        ],
        "listAccountProposals": [
          "dashboard:read",
        ],
        "listAccounts": [
          "dashboard:read",
        ],
        "listGlobalDeltas": [
          "dashboard:read",
        ],
        "listGlobalProposals": [
          "dashboard:read",
        ],
      }
    `);
  });

  it('is frozen so consumers cannot mutate the map by accident', () => {
    expect(Object.isFrozen(REQUIRED_PERMISSIONS)).toBe(true);
  });

  it('exposes the three v1 permission consts as the expected wire strings', () => {
    expect(DASHBOARD_READ).toBe('dashboard:read');
    expect(ACCOUNTS_PAUSE).toBe('accounts:pause');
    expect(POLICIES_WRITE).toBe('policies:write');
  });
});

describe('requiredPermissionsFor', () => {
  it('returns the permissions array for a known endpoint', () => {
    expect(requiredPermissionsFor('listAccounts')).toEqual(['dashboard:read']);
  });

  it('returns null for an unknown endpoint key', () => {
    // Forces the runtime path that the TS compiler would normally
    // reject; mirrors what a stale-client / new-server interaction
    // would look like.
    expect(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      requiredPermissionsFor('totallyMadeUpEndpoint' as any),
    ).toBeNull();
  });
});
