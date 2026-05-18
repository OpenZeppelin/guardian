/**
 * Per-endpoint required-permission metadata for the operator dashboard
 * (feature `006-operator-authz` §FR-030 / FR-031).
 *
 * Dashboards MAY consult this metadata to gate UI affordances — e.g.
 * hide a "Pause account" button when the authenticated operator does
 * not hold `accounts:pause`. The metadata is **advisory only**: the
 * server's authorization middleware is the source of truth (FR-032).
 * The TypeScript client MUST NOT short-circuit a request based on the
 * metadata alone; every call still reaches the server.
 *
 * Naming the keys matches the wrapped method names on
 * `GuardianOperatorHttpClient`. Adding a new method must add a key
 * here in the same commit; the Vitest snapshot covers drift.
 */

/**
 * Stable wire strings for each v1 permission. Match
 * `crates/server/src/dashboard/permissions.rs::Permission::as_str`.
 */
export const DASHBOARD_READ = 'dashboard:read' as const;
export const ACCOUNTS_PAUSE = 'accounts:pause' as const;
export const POLICIES_WRITE = 'policies:write' as const;

/** Union of v1 permission strings. */
export type OperatorPermission =
  | typeof DASHBOARD_READ
  | typeof ACCOUNTS_PAUSE
  | typeof POLICIES_WRITE;

/**
 * Logical keys for each endpoint the TS client wraps. Strings are
 * stable identifiers used by dashboard UIs to look up the required
 * permission set; renaming a key is a breaking change to consumers.
 */
export type DashboardEndpointKey =
  | 'listAccounts'
  | 'getAccount'
  | 'getAccountSnapshot'
  | 'listAccountDeltas'
  | 'listAccountProposals'
  | 'getDashboardInfo'
  | 'listGlobalDeltas'
  | 'listGlobalProposals';

/**
 * Map from endpoint key to the required permission set the server's
 * authorization middleware enforces on that route. Lists are
 * lexicographically sorted (`dashboard:read` before `accounts:pause`)
 * so snapshot tests are stable.
 */
export const REQUIRED_PERMISSIONS: Readonly<
  Record<DashboardEndpointKey, ReadonlyArray<OperatorPermission>>
> = Object.freeze({
  listAccounts: [DASHBOARD_READ] as const,
  getAccount: [DASHBOARD_READ] as const,
  getAccountSnapshot: [DASHBOARD_READ] as const,
  listAccountDeltas: [DASHBOARD_READ] as const,
  listAccountProposals: [DASHBOARD_READ] as const,
  getDashboardInfo: [DASHBOARD_READ] as const,
  listGlobalDeltas: [DASHBOARD_READ] as const,
  listGlobalProposals: [DASHBOARD_READ] as const,
});

/**
 * Returns the permissions required for a given endpoint, or `null` if
 * the key is unknown. Type-narrowed `null`-return is preferable to
 * throwing because the caller usually wants to fall back to "permit
 * the click; let the server decide" when the client is older than the
 * server.
 */
export function requiredPermissionsFor(
  endpoint: DashboardEndpointKey,
): ReadonlyArray<OperatorPermission> | null {
  return REQUIRED_PERMISSIONS[endpoint] ?? null;
}
