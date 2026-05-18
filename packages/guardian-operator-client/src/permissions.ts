/**
 * Stable wire strings for the v1 operator permission vocabulary
 * (feature `006-operator-authz` §FR-004). Mirrors
 * `crates/server/src/dashboard/permissions.rs::Permission::as_str`.
 *
 * Dashboards read the authenticated operator's live permission set
 * from `GET /dashboard/session` and compare each entry against these
 * constants for typed set-membership checks.
 */

export const DASHBOARD_READ = 'dashboard:read' as const;
export const ACCOUNTS_PAUSE = 'accounts:pause' as const;
export const POLICIES_WRITE = 'policies:write' as const;

/** Union of v1 permission strings, useful for typed UI checks. */
export type OperatorPermission =
  | typeof DASHBOARD_READ
  | typeof ACCOUNTS_PAUSE
  | typeof POLICIES_WRITE;
