# Feature Specification: Operator Authorization Foundation — Per-Operator Permissions, Enforcement, and Mutating-Action Audit

**Feature Key**: `006-operator-authz`
**Suggested Branch**: `006-operator-authz` (manual creation optional)
**Created**: 2026-05-15
**Status**: Draft
**Input**: User description: "Operator authorization foundation: extend the existing operator allowlist JSON to carry per-entry permissions, add middleware enforcement, an always-on admin_actions audit writer, and a GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION error code — minimum needed to unblock account pause ([#181](https://github.com/OpenZeppelin/guardian/issues/181)) and future mutating operator endpoints."
**Related**:
- Unblocks [#181](https://github.com/OpenZeppelin/guardian/issues/181) (Account Pause), [#182](https://github.com/OpenZeppelin/guardian/issues/182) (Policy Evaluation).
- Builds on [`002-operator-auth`](../002-operator-auth/spec.md) (identity-only operator session).
- **Follow-up (not in this feature)**: DB-backed operator storage and dashboard endpoints for operator/permission management — tracked as a separate ticket (see §Dependencies).
- Soft prerequisite for [#179](https://github.com/OpenZeppelin/guardian/issues/179) (structured Guardian error contract); this feature introduces one code in that family.

## Context

Guardian's existing operator surface ([`002-operator-auth`](../002-operator-auth/spec.md),
[`003-operator-account-apis`](../003-operator-account-apis/spec.md),
[`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/spec.md))
authenticates operators via challenge/sign + an opaque server-side session
cookie. Sessions are held in-process in
`Arc<Mutex<HashMap<...>>>` (`crates/server/src/dashboard/state.rs:27-28`);
there is **no operator-side database today** — the only Postgres-backed
storage in the repo is the multisig-state metadata DB
(`crates/server/src/storage/postgres.rs` + `crates/server/migrations/`).
The operator allowlist is loaded from a JSON file
(`GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE`) or AWS Secrets Manager secret
(`GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID`) and reloaded on every
authentication request (`crates/server/src/dashboard/allowlist.rs:42-117`,
`crates/server/src/dashboard/state.rs:369-390`). At deploy time the
same JSON also flows through `scripts/aws-deploy.sh:292-293` as
`GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` — Terraform input that becomes the
AWS Secrets Manager secret's payload — so all three surfaces share one
JSON schema. The current JSON shape is a flat `Vec<String>` of hex
Falcon public keys (`allowlist.rs:125` `from_json`). The authenticated
principal today (`crates/server/src/dashboard/types.rs:6-10`,
`AuthenticatedOperator { operator_id, commitment }`) carries identity
only — `operator_id` is populated as the commitment hex but the type
already has a separate slot for a human identifier. The session
middleware (`crates/server/src/dashboard/middleware.rs:9-24`) attaches
that identity to the request context and nothing else; every
allowlisted operator has equal capability on every dashboard endpoint.
This is sufficient for read-only dashboard GETs because the only
access decision is "is this caller a Guardian operator at all".
The operator HTTP surface is served via Axum; there is no operator
gRPC surface today (`crates/server/proto/guardian.proto:6-42`
defines only account/state RPCs).

[#181](https://github.com/OpenZeppelin/guardian/issues/181) (account pause)
is the first **mutating** operator endpoint, and [#182](https://github.com/OpenZeppelin/guardian/issues/182)
(operator-driven policy enable/disable/parameter updates) is the next.
"Any logged-in operator can pause any account" is not an acceptable
default — pause is a kill switch that rejects state-mutating actions
across the multisig protocol, and policy toggles can relax or tighten
platform-level safety. Both need **authorization** in addition to
authentication: a permission set bound to the operator key, enforced
after session validation, denied with a stable error code, and recorded
in an append-only forensic trail so security review and incident
response can answer "who paused this account, who granted this
permission, who toggled this policy, and when" without resorting to
application logs.

This feature introduces the operator authorization layer in isolation,
ahead of any mutating consumer endpoint. The design lands the **runtime
plumbing** that #181 and #182 need (permissions on the principal,
middleware, error code, audit writer) but deliberately **does not**
introduce database-backed operator storage or dashboard endpoints for
operator/permission CRUD. Those land in a follow-up so this feature
stays close to "minimum needed to unblock #181".

The design uses a single, uniform operator source — the existing
allowlist JSON, extended in place. Each JSON array element becomes
**either** a hex string (legacy, read-only) **or** an object
`{ "public_key", "permissions" }` (explicit permission set). The
schema change applies to every surface that ingests the allowlist
JSON: the deploy-time `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` variable,
the file source, and the AWS Secrets Manager secret payload. There
is no separate "admin" env var, no source-precedence rule, and no
shadowing semantics. To bootstrap a pause-capable operator before
[#181](https://github.com/OpenZeppelin/guardian/issues/181) ships, a
deployment edits the same allowlist source it already uses and
promotes one entry from a string to an object with the desired
permissions.

The change also introduces:

- An authorization middleware that runs **after** session validation
  and denies routes whose required permission set is not satisfied,
  with `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` as a new stable
  error code.
- A new **always-on** `admin_actions` audit writer that records
  successful and rejected mutating-action attempts. The writer
  persists to a new table in the existing multisig-state Postgres DB
  when Postgres is available, and **falls back to structured log
  output** otherwise (one log line per event, tagged for log-
  collection scraping, with a loud startup warning that audit is
  not persisted). There is no feature flag to disable audit; the
  writer is always invoked.
- A `cfg`/Cargo-feature-gated probe endpoint so the middleware is
  end-to-end testable before #181 lands.
- Extensions to `@openzeppelin/guardian-operator-client` so dashboards
  detect the new error code through the **existing**
  `DashboardErrorCode` typed union (`packages/guardian-operator-client/src/http.ts:45-167`),
  not by parsing HTTP status or English strings.

The change does **not** alter the multisig protocol, the proposal
lifecycle, the existing per-account authenticated APIs, or the
read-only dashboard endpoints' wire shapes. Behavior across supported
metadata/state backends is preserved.

## Scope *(mandatory)*

### In Scope

**Allowlist permissions (uniform JSON schema)**

- Extend the existing allowlist JSON schema so each array element is
  either:
  - a hex string (e.g. `"0x094f..."`) — legacy-grant, treated as
    holding `{dashboard:read}` only, **or**
  - an object `{ "public_key": "0xhex", "permissions": ["..."] }` —
    explicit permission set.
- The schema change applies uniformly to the three loaders that share
  this JSON:
  - `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` deploy variable (Terraform →
    AWS Secrets Manager payload, per `scripts/aws-deploy.sh:292-293`),
  - `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` file source
    (`allowlist.rs::from_env` → `from_json`),
  - `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID` AWS Secrets Manager
    payload (same `from_json` parser).
- Both array element shapes may coexist within one JSON document.
- Define a small, stable, namespaced permission vocabulary for this
  feature: `dashboard:read`, `accounts:pause`, `policies:write`.
  No additional permissions are introduced in v1; new permissions are
  reserved for the consumer features that need them (e.g. [#181](https://github.com/OpenZeppelin/guardian/issues/181)
  is the first user of `accounts:pause`).

**Request context**

- Extend the authenticated-operator principal so handlers and
  middleware can read the operator's effective permission set in
  addition to the identity (`operator_id` + `commitment`) that
  `AuthenticatedOperator` already carries.

**Authorization middleware**

- Add a second middleware layer that runs **after** the existing
  `require_dashboard_session` and any route can declare a required
  permission set against. The middleware denies with the new stable
  error code when the operator's permission set does not satisfy the
  required set, and emits an audit row for the denied attempt via the
  shared writer.
- Apply the new authorization middleware retroactively to every
  existing dashboard read endpoint with the required permission set
  `{dashboard:read}`. This is a behavior change only for operators
  whose allowlist entry is an object with `permissions: []`; legacy
  hex-string entries continue to pass.

**Audit (`admin_actions`) — always-on**

- Introduce a single append-only `admin_actions` table with the
  minimal schema `(id, occurred_at, operator_identity, action_kind,
  target_account_id NULL, payload JSONB, outcome, error_code NULL)`
  and a shared `Auditor::record(event)` writer used by the
  authorization middleware (for rejections) and reserved for use by
  future mutating endpoints (for both success and failure rows).
- The writer is **always invoked** on every audit event; there is no
  feature flag to disable it.
- When the multisig-state Postgres DB is configured, the writer
  persists rows to the new `admin_actions` table. When Guardian runs
  on a non-Postgres metadata backend (filesystem-only deployments),
  the writer emits one structured log line per event (with a known,
  greppable selector) and emits a loud startup warning that audit
  events are not persisted.

**Error contract**

- Add one new stable error code `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`
  with pinned HTTP status (`403 Forbidden`), plus a stable response
  shape including the permission(s) the operator lacks. The new code
  rides the existing flat `ErrorResponse` envelope additively (see
  §Design decisions); FR-016 has the exact field-level contract.
- Pinning the human-readable `error` field is out of scope.
- No gRPC mapping is introduced because the operator surface is
  HTTP-only today.

**TypeScript client**

- Extend `@openzeppelin/guardian-operator-client` so the existing
  `DashboardErrorCode` union (`packages/guardian-operator-client/src/http.ts:45`)
  gains a new variant for permission denial, and the existing
  `parseErrorBody` (`http.ts:78-129`) populates `missing_permissions`
  for that code.
- Dashboards gate UI affordances against the **live** operator
  permission set returned by `GET /dashboard/session` (US6), not
  against a static client-side map. The server remains authoritative
  on every call.

**Session introspection endpoint**

- Add a single read-only endpoint `GET /dashboard/session` that returns the
  authenticated operator's identity and effective permission set. The
  endpoint requires a valid operator session but **no specific permission**
  (so operators with `permissions: []` can still call it and receive their
  empty set rather than `403`). Dashboards consult this endpoint to gate UI
  affordances against the operator's **actual** capabilities; capability
  gating in dashboards goes through this endpoint. Because permissions
  are re-resolved from the
  allowlist on every authenticated request (FR-008), this endpoint is also
  the natural polling point for hot-reloaded permission changes without
  forcing re-login.
- The endpoint is **not** recorded in `admin_actions`. It is a read-only
  metadata endpoint expected to be polled by dashboards; auditing it would
  flood the forensic stream with non-mutating noise inconsistent with the
  rest of the dashboard read surface.

**Probe endpoint (testing aid)**

- Introduce exactly one operator-authenticated probe endpoint to
  exercise the authorization middleware end-to-end in tests and the
  operator smoke harness without waiting on [#181](https://github.com/OpenZeppelin/guardian/issues/181).
  The probe endpoint declares a required permission set of
  `{accounts:pause}` and, on success, performs no state change beyond
  writing one `admin_actions` row. The probe is gated by a Cargo
  feature whose default is **off** in release builds, so it cannot
  ship to production accidentally.

**Edge-case determinism**

- Provide deterministic, explicit outcomes for allowlist entries with
  unknown or malformed permission strings, including hot-reload paths
  inherited from [`002-operator-auth`](../002-operator-auth/spec.md).

### Out of Scope

- **DB-backed operator storage**. There is no `operators` table, no
  migration from the JSON allowlist into a database, and no runtime
  mutation of operator entries through the dashboard in this feature.
  Tracked separately (see §Dependencies follow-up ticket).
- **Operator / permission management endpoints**. Dashboard CRUD
  ("add an operator", "grant a permission", "revoke a permission") is
  the follow-up feature. v1 manages operators by editing the existing
  allowlist JSON source (env-var-into-secret, file, or secret-id) and
  triggering reload as today.
- **Any feature flag to disable audit.** The `Auditor` is always
  invoked; non-Postgres deployments degrade to structured log output
  rather than no-op. There is no "audit disabled" configuration.
- **A separate env-admin variable.** The original draft introduced
  `GUARDIAN_OPERATOR_ADMIN_KEYS` as a bootstrap shortcut; this feature
  drops it in favor of the heterogeneous-JSON schema described above.
- Any actual mutating consumer endpoint. Account pause
  ([#181](https://github.com/OpenZeppelin/guardian/issues/181)) and
  policy write ([#182](https://github.com/OpenZeppelin/guardian/issues/182))
  land in their own features and are the first real consumers of this
  authorization layer.
- Role definitions, role CRUD APIs, or any mapping from "role" to
  "permission set". The allowlist holds raw permission strings per
  operator in v1.
- A general policy DSL or per-evaluation policy decision log. The
  `admin_actions` table records mutating operator actions only;
  per-evaluation co-sign / delta application policy decisions are
  **not** written here ([#182](https://github.com/OpenZeppelin/guardian/issues/182)
  reuses the same audit writer for operator-driven policy toggles;
  per-evaluation decisions remain structured-log only — the M4 work
  flagged in the parent architecture document).
- A queryable `admin_actions` read endpoint. v1 ships a writer and a
  table only; operators inspect rows via existing DB tooling (`psql` /
  equivalents) or via the log fallback selector.
- A retention or pruning policy for `admin_actions`. Expected volume is
  order-of-magnitude tens of rows per day per deployment, so v1 keeps
  rows indefinitely and defers retention decisions until volume warrants
  one.
- Pinning the human-readable `error` field for the new error code or
  localizing it. Clients key off `code`; `error` is best-effort English.
- A general structured-error refactor across every existing Guardian
  failure mode. This feature introduces exactly one new code (the
  permission denial); the broader error-model migration from [#179](https://github.com/OpenZeppelin/guardian/issues/179)
  remains its own feature.
- Audit-event ingestion into an external SIEM / log aggregator. v1
  lands the DB row + log-line dual surface; external pipeline tooling
  is a follow-up.
- Per-account permission scoping (e.g. "operator X may pause only
  account Y"). v1 permissions apply server-wide; per-target scoping is
  deferred.
- Multi-tenant operator partitioning. The allowlist remains
  Guardian-instance-global as in [`002-operator-auth`](../002-operator-auth/spec.md).
- gRPC parity for operator endpoints. The operator surface is
  HTTP-only today (`crates/server/proto/guardian.proto` exposes only
  account/state RPCs); this feature does not add gRPC for the new
  middleware path.
- Rate limiting or anomaly detection on denied permission attempts.
  Denials are audited but not throttled or alerted in v1.
- Multi-replica session sharing. Sessions remain in-process
  (`Arc<Mutex<HashMap<...>>>`) as today; a permission change applies
  on the replica that handles the next request from that operator
  (see §Assumptions).

## User Scenarios & Testing *(mandatory)*

These behaviors sit on the existing dashboard HTTP surface and rely on
the operator session established by [`002-operator-auth`](../002-operator-auth/spec.md).
Validation is primarily through server integration tests and the
`guardian-operator-client` typed wrappers; the gated probe endpoint
stands in for a real mutating consumer until
[#181](https://github.com/OpenZeppelin/guardian/issues/181) lands.

### User Story 1 - Read-Only Operator Can Still Use The Dashboard (Priority: P1)

As an existing operator whose allowlist entry predates this feature, I
continue to be able to use every dashboard read endpoint without any
allowlist change so this feature does not break already-deployed
Guardians.

**Why this priority**: This is the backwards-compatibility guarantee.
Without it, every existing deployment loses dashboard access the moment
the authorization layer ships.

**Independent Test**: Configure an allowlist whose entries are all hex
strings (the legacy `Vec<String>` shape), establish a valid operator
session against the existing dashboard, and verify every read endpoint
introduced through
[`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/spec.md)
still returns its existing payload unchanged.

**Acceptance Scenarios**:

1. **Given** a valid operator session and an allowlist entry that is a
   bare hex string, **When** the operator calls any existing dashboard
   read endpoint, **Then** the server treats the operator as holding
   `{dashboard:read}`, the request succeeds, and the response payload
   is semantically identical to the pre-feature contract — same
   schema, same field values, same HTTP status — verified by replaying
   the pre-feature
   [`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/spec.md)
   integration suite without modification.
2. **Given** a valid operator session and an allowlist entry of the
   form `{"public_key": "0xhex", "permissions": ["dashboard:read"]}`,
   **When** the operator calls any existing dashboard read endpoint,
   **Then** the request succeeds with the same response payload as
   Scenario 1.
3. **Given** a valid operator session and an allowlist entry of the
   form `{"public_key": "0xhex", "permissions": []}`, **When** the
   operator calls any dashboard read endpoint, **Then** the server
   denies with `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` and HTTP
   `403`. Explicit empty set is a permission revocation, not
   legacy-grant.

---

### User Story 2 - Allowlist-Only Operator Without The Mutating Permission Is Denied (Priority: P1)

As a security-conscious operator deployment, I can grant a subset of
allowlisted operators the ability to perform mutating actions while
keeping the rest read-only, so the principle of least privilege
applies to pause and policy controls before they ship.

**Why this priority**: This is the load-bearing reason the
authorization layer exists. If a legacy-grant allowlist operator can
hit the gated probe (or, later, account pause), the model has failed.

**Independent Test**: Configure two allowlist entries — one legacy
hex string and one
`{"public_key": "0xhex", "permissions": ["dashboard:read", "accounts:pause"]}`.
Establish a valid session for each. Call the gated probe endpoint
declaring required permission `{accounts:pause}` from both sessions.
Verify the legacy-grant session receives
`GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` / HTTP `403`, and the
permissioned session receives success and writes one `admin_actions`
row recording the success.

**Acceptance Scenarios**:

1. **Given** a valid operator session whose allowlist entry does not
   include `accounts:pause`, **When** the operator calls the gated
   probe endpoint, **Then** the server denies with
   `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`, HTTP `403`, the
   response includes the missing permission(s) in a stable
   `missing_permissions` field, and the audit writer records one
   `admin_actions` row with `outcome = denied` and
   `error_code = GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`.
2. **Given** a valid operator session whose allowlist entry includes
   `accounts:pause`, **When** the operator calls the gated probe
   endpoint, **Then** the server returns success and the audit writer
   records one `admin_actions` row with `outcome = success` and
   `error_code = NULL`.
3. **Given** no valid operator session at all, **When** the caller
   invokes the gated probe endpoint, **Then** the server returns
   `401 Unauthorized` from the existing session layer **before** the
   authorization middleware runs, and **no** `admin_actions` event is
   written. Authentication failures are not authorization audit events.
4. **Given** the gated probe endpoint Cargo feature is disabled
   (release build default), **When** any operator calls it, **Then**
   the server returns `404 Not Found` and writes no audit event. The
   probe is a testing aid, not a production surface.

---

### User Story 3 - Permission Changes Take Effect Without Server Restart (Priority: P2)

As a Guardian deployment operator, I can grant or revoke a permission
on an existing allowlist entry the same way I add or remove an operator
today, so promoting an operator from read-only to pause-capable does
not require a restart and a redeploy.

**Why this priority**: [`002-operator-auth`](../002-operator-auth/spec.md)
already reloads the allowlist on every authentication request
(`crates/server/src/dashboard/state.rs:369-390`). Threading permissions
through that same path keeps the operational story unchanged. If
permission changes required a restart, deployments would have to
choose between security agility and uptime.

**Independent Test**: Start Guardian with an allowlist operator entry
holding `permissions: ["dashboard:read"]`. Verify that operator's
gated probe call is denied. Edit the allowlist source (file or
secret) to add `accounts:pause` and trigger the existing reload path.
Re-issue the probe call within the same session and verify it now
succeeds and that one `admin_actions` success event was written.

**Acceptance Scenarios**:

1. **Given** an active operator session and an allowlist edit that
   adds a permission to that operator after the session was issued,
   **When** the next request from that session arrives (triggering
   the existing reload-on-authenticate path), **Then** the new
   permission takes effect on the existing session without requiring
   re-login.
2. **Given** an active operator session and an allowlist edit that
   removes a permission from that operator after the session was
   issued, **When** the next request requiring the removed permission
   arrives, **Then** the server denies with
   `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`. Read capability (and
   the session itself) remain valid unless the entry was removed
   entirely (existing [`002-operator-auth`](../002-operator-auth/spec.md)
   semantics). **Note**: revocation of an individual permission does
   NOT invalidate the session; only full allowlist removal does.
3. **Given** an allowlist edit that introduces a permission string
   the server does not recognize, **When** the reload runs, **Then**
   the reload fails with a deterministic error and the previously
   loaded allowlist is retained for new requests (see §Edge Cases for
   the existing reload-failure behavior on `ConfigurationError`). The
   server MUST NOT silently treat the unknown permission as a no-op
   or as a wildcard grant.

---

### User Story 4 - Mutating Attempts Are Forensically Traceable (Priority: P2)

As a security reviewer or incident responder, I can reconstruct who
attempted a mutating operator action, when, against which target, and
whether it succeeded or was denied, by reading rows from a single
append-only table (or, for non-Postgres deployments, a single
greppable log selector) without correlating across application logs.

**Why this priority**: The architecture document calls out that
"who paused this account / who granted this permission" must be
answerable forensically; rows-not-logs is the contract that lets
audit and incident response work without log-aggregation tooling
where the DB exists, and the log fallback ensures non-Postgres
deployments still surface the same events to anyone collecting
stdout/stderr.

**Independent Test**: Drive a mix of permissioned and unpermissioned
operators through the gated probe endpoint. After each call, query
the `admin_actions` table directly (Postgres deployments) or grep
for the audit log selector (non-Postgres deployments) and verify
exactly one event per attempt with operator identity, timestamp,
action kind, outcome, and (on failure) the error code.

**Acceptance Scenarios**:

1. **Given** a successful mutating-action attempt, **When** the writer
   completes, **Then** exactly one `admin_actions` event exists for
   that attempt with `outcome = success`, `error_code = NULL`, the
   operator identity (`operator_id`) from the authenticated session,
   a stable `action_kind` string, an `occurred_at` timestamp set
   server-side, and (when applicable) a `target_account_id`. In
   Postgres deployments the event is a row; in non-Postgres
   deployments the event is a structured log line carrying the same
   fields under a known selector.
2. **Given** a denied mutating-action attempt due to insufficient
   permission, **When** the middleware completes, **Then** exactly one
   `admin_actions` event exists with `outcome = denied`, `error_code =
   GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`, and the same identity /
   timestamp / kind / target fields populated as the success case.
3. **Given** a transient database failure when the audit writer tries
   to persist a denial row (Postgres deployments only), **When** the
   writer fails to insert, **Then** the server still returns the
   denial response to the operator AND the writer emits the same
   structured log line it would have emitted on a non-Postgres
   deployment, so the gap is detectable; the operator response is not
   blocked on the row landing.
4. **Given** an `admin_actions` row that is persisted, **When** any
   process attempts to UPDATE or DELETE it through normal server
   operation, **Then** the operation fails. Enforcement is a Postgres
   trigger on `admin_actions` (see §Design decisions); log-fallback
   events are append-only by construction.

---

### User Story 5 - Dashboard Can Distinguish Denial From Other Errors (Priority: P3)

As a dashboard developer integrating against the operator client, I
can detect a permission denial as a typed discriminator on the
returned error so the UI can render "you don't have permission to do
this" instead of a generic failure, and so the UI can hide actions
the operator cannot perform.

**Why this priority**: The TS client already has a typed
`DashboardErrorCode` union and a parsed `GuardianOperatorHttpError`
(`packages/guardian-operator-client/src/http.ts:45-167`); this feature
extends that union. Without typed surfacing, dashboards would fall
back to parsing HTTP status codes or English strings, neither of
which is durable.

**Independent Test**: From a TypeScript test against
`@openzeppelin/guardian-operator-client`, invoke the gated probe
endpoint from a session lacking `accounts:pause` and verify the
rejected promise carries a typed error whose `code` is exactly the
new permission-denial variant of `DashboardErrorCode` and whose
parsed data lists the missing permission(s). Verify the same call
from a permissioned session resolves successfully.

**Acceptance Scenarios**:

1. **Given** a permission denial response, **When** the operator
   client surfaces the error, **Then** consumers see a typed
   `DashboardErrorCode` variant (e.g. `insufficient_operator_permission`,
   matching whatever naming convention the existing union already
   uses) and structured `missing_permissions: string[]`; the human
   `error` field is not part of the typed contract.
2. **Given** the operator client exposes per-endpoint required
   permission metadata, **When** a dashboard consults that metadata,
   **Then** the metadata matches the server's actual middleware
   requirement for the same endpoint; the dashboard does not need to
   hardcode permission strings.
3. **Given** any other Guardian error (e.g. `401 Unauthorized`, `404`,
   `500`), **When** the operator client surfaces it, **Then** the
   error is **not** typed as the new permission-denial variant; the
   new code is reserved exclusively for the authorization middleware's
   denial path.

---

### User Story 6 - Dashboard Can Discover Current Operator's Permissions (Priority: P3)

As a dashboard developer integrating against the operator client, I can
ask the server "who is the current operator and what can they do" with a
single call so the UI shows only the actions the operator is actually
permitted to perform, instead of hiding behind a static client-side map
that can drift from the server's middleware requirements.

**Why this priority**: This is a dashboard ergonomics win, not a security
boundary — the server middleware (US2) remains the source of truth, and
the typed denial error (US5) is what protects users when client-side
gating is wrong or stale. Without this endpoint, dashboards would have to
call every endpoint and react to `403` (poor UX). With it, dashboards
get a single live read of the operator's effective permission set that
naturally tracks allowlist hot-reloads via FR-008.

**Independent Test**: Establish a valid operator session for an entry with
`permissions: ["dashboard:read", "accounts:pause"]`. Call
`GET /dashboard/session` and verify the response carries the operator
identity and exactly that permission set in lexicographic order. Edit the
allowlist to remove `accounts:pause`, trigger a reload, re-call the
endpoint within the same session, and verify the response now reflects
only `["dashboard:read"]` without re-login. Establish a second session
for an entry with `permissions: []` and verify the endpoint returns
`200` with an empty permission array (not `403`).

**Acceptance Scenarios**:

1. **Given** a valid operator session whose allowlist entry holds a
   non-empty permission set, **When** the operator calls
   `GET /dashboard/session`, **Then** the server returns `200 OK` with
   a body containing the `operator_id` from the authenticated principal
   and the effective permission set as a lexicographically ordered
   string array.
2. **Given** a valid operator session whose allowlist entry holds
   `permissions: []`, **When** the operator calls
   `GET /dashboard/session`, **Then** the server returns `200 OK` with
   the operator identity and an empty `permissions` array. The
   endpoint MUST NOT deny on the empty-permission set case — this is
   the load-bearing UX property that lets the dashboard distinguish
   "no permissions" from "not logged in".
3. **Given** an active session and a mid-session allowlist edit that
   grants or revokes a permission, **When** the operator next calls
   `GET /dashboard/session`, **Then** the response reflects the new
   effective permission set without requiring re-login (via the
   FR-008 re-resolve path).
4. **Given** no valid operator session, **When** the caller invokes
   `GET /dashboard/session`, **Then** the server returns `401
   Unauthorized` from the existing session layer. No `admin_actions`
   event is written for the authenticated success case OR the
   unauthenticated failure case.

---

## Requirements *(mandatory)*

### Functional Requirements

**Allowlist & permission model**

- **FR-001**: The operator allowlist JSON schema (consumed identically
  by `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` deploy variable,
  `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE`, and
  `GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID`) MUST accept a
  heterogeneous JSON array whose elements are **either** (a) a hex
  string of a Falcon public key, or (b) a JSON object with exactly
  the keys `public_key` and `permissions`. Both shapes MAY coexist
  within one document. Object entries with any unknown additional
  property (e.g. `comment`, `role`, future schema extensions) MUST
  be rejected with a deterministic load error — strict parsing,
  consistent with FR-004's stance on unknown permission strings.
- **FR-002**: An array element that is a bare hex string MUST be
  treated as holding exactly `{dashboard:read}` and no other
  permission. This is the legacy-grant rule that preserves existing
  read-only behavior for unmodified deployments.
- **FR-003**: An array element of the object shape with
  `permissions: []` (explicit empty array) MUST be treated as
  holding **no** permissions and MUST be denied on every endpoint
  that requires any permission. Explicit empty is **not**
  legacy-grant.
- **FR-004**: The server MUST recognize exactly the following
  permission strings in v1: `dashboard:read`, `accounts:pause`,
  `policies:write`. Any other string MUST cause a deterministic
  allowlist-load rejection (startup error or hot-reload error,
  matching the existing `ConfigurationError` propagation path from
  `dashboard/state.rs:374`). Unknown permissions MUST NOT silently
  load as a no-op or as a wildcard. **Note (downgrade trap):**
  rolling back to a server build that predates a granted permission
  will fail to load; this is the trade for typo-protection.
  Deployments must remove forward-defined permissions from the
  allowlist before downgrading.
- **FR-005**: Permission strings MUST be matched case-sensitively. A
  permission of `Accounts:Pause` MUST be rejected as unknown rather
  than coerced to `accounts:pause`. Leading/trailing whitespace
  inside a permission string MUST also be rejected as unknown.
- **FR-006**: Duplicate permissions in one object entry MUST load
  successfully and be treated as the deduplicated set. Duplicates
  are not an authoring error.
- **FR-007**: A duplicate `public_key` across array elements (whether
  string-or-object) MUST cause a deterministic load rejection.
  Resolving ambiguous duplicates by "last wins" or "union of
  permissions" silently is forbidden — the operator must edit the
  source.

**Request context**

- **FR-008**: After successful session validation, the authenticated
  operator request context MUST expose the operator identity (already
  present today) plus the effective permission set drawn from the
  currently loaded allowlist at the moment of the authentication
  call. **Every operator-authenticated request MUST run
  `authenticate_session` (or its equivalent successor entrypoint) so
  the principal's permission set is re-resolved from the live
  allowlist snapshot on every request — no route may bypass this
  path.** Hot-reload of allowlist permissions takes effect on the
  next authenticated request because
  `crates/server/src/dashboard/state.rs::authenticate_session`
  already refreshes the allowlist; FR-008 makes that property
  load-bearing rather than incidental, so the implementation must
  preserve it for any new operator route.
- **FR-009**: A handler MUST be able to read the operator's
  permission set from the request context without consulting the
  allowlist or session store again.

**Authorization middleware**

- **FR-010**: Routes MUST declare their required permission set at
  registration time (in
  `crates/server/src/builder/handle.rs` alongside the dashboard
  `Router`). A route declaring required permission `P` MUST be
  rejected for any session whose effective permission set does not
  contain `P`.
- **FR-011**: The required permission set MAY contain multiple
  permissions; the middleware MUST require **all** declared
  permissions to be present (conjunction). v1 has no need for
  disjunctive ("any of") requirements, and disjunction MUST NOT be
  introduced in v1.
- **FR-012**: The middleware MUST run **after** session
  authentication (`require_dashboard_session`). Missing or invalid
  sessions MUST continue to produce the existing `401 Unauthorized`
  response without invoking the authorization middleware and without
  writing an audit event.
- **FR-013**: On denial, the middleware MUST return HTTP `403
  Forbidden` and a body conforming to the new error code shape (see
  FR-015..FR-017). The denial response MUST NOT reveal the
  existence, non-existence, or permissions of any operator other
  than the caller. Revealing what the **route** requires is
  permitted and expected.
- **FR-014**: Every existing dashboard read endpoint registered under
  the session layer (accounts list, account detail, snapshot, info,
  feeds, etc.) MUST declare required permission set `{dashboard:read}`.
  Endpoints that are not operator-authenticated MUST NOT be affected.

**Error contract**

- **FR-015**: The server MUST introduce one new stable error code
  `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` with pinned HTTP status
  `403 Forbidden`. The code string MUST be stable across releases;
  renaming it is a breaking contract change.
- **FR-016**: The error response body MUST include at least the
  top-level `code = "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION"`, a
  human `error` field (best-effort English; not part of the typed
  contract), a `missing_permissions` list of the permission strings
  the route required that the operator lacks, and a `retryable =
  false` indicator (a permission denial is not transient). The new
  fields are additive extensions of the existing flat envelope
  (`crates/server/src/error.rs::ErrorResponse`); see §Design
  decisions for rationale.
- **FR-017**: The `missing_permissions` array MUST be ordered
  deterministically (lexicographic ASCII) so snapshot tests and
  audit events do not drift between runs.

**Audit (`admin_actions`) — always-on**

- **FR-018**: The server MUST persist a new append-only table named
  `admin_actions` with at least the columns `(id, occurred_at,
  operator_identity, action_kind, target_account_id NULL, payload
  JSONB, outcome, error_code NULL)`. `outcome` MUST be one of
  `success` or `denied`. Schema MUST live in the existing
  multisig-state Postgres DB (a new migration under
  `crates/server/migrations/`). Indexes: `(operator_identity,
  occurred_at DESC)` and `(occurred_at DESC)`.
- **FR-019**: All audit writes MUST go through a single
  `Auditor::record(event)` writer (a new Rust trait/service under
  `crates/server/src/dashboard/` or `crates/server/src/audit/`).
  The authorization middleware MUST call it for every denial.
  Future mutating endpoints (starting with
  [#181](https://github.com/OpenZeppelin/guardian/issues/181)) MUST
  call it for every success and every failure they own. No feature
  may implement its own audit shape.
- **FR-020**: The `Auditor` MUST be **always-invoked**. There MUST
  NOT be a feature flag, env var, or runtime toggle that disables
  it. Deployments that do not want audit data persisted can ignore
  the rows / logs; they cannot suppress emission.
- **FR-021**: For deployments configured with the multisig-state
  Postgres DB, the `Auditor` MUST persist each event as a row in
  `admin_actions`. For deployments running on a non-Postgres
  metadata backend, the `Auditor` MUST emit each event as a single
  structured log line under a known, greppable selector (working
  name `audit.admin_action`), carrying the same fields as the
  Postgres row, AND the server MUST emit a loud one-shot startup
  warning that audit events are not persisted.
- **FR-022**: `occurred_at` MUST be assigned server-side. Client- or
  caller-supplied timestamps MUST NOT be honored.
- **FR-023**: `operator_identity` MUST be the `operator_id` field of
  the existing `AuthenticatedOperator`
  (`crates/server/src/dashboard/types.rs`), which today is the
  commitment hex string. The encoding is `TEXT` hex (matching the
  existing `commitment` field encoding) so audit queries via `psql`
  remain ergonomic and the log-line fields match the Postgres
  column shape exactly.
- **FR-024**: `action_kind` MUST be a small stable string from a
  server-controlled vocabulary registered in a single Rust module
  (working name `crates/server/src/audit/kinds.rs` or equivalent).
  v1 reserves `auth.denied` for middleware-recorded rejections and
  `probe.access` for the gated probe endpoint. Future mutating
  endpoints register their own kinds in the same module (e.g.
  [#181](https://github.com/OpenZeppelin/guardian/issues/181) will
  register `accounts.pause` / `accounts.unpause`).
- **FR-025**: The `payload JSONB` column (and the matching log-line
  field) MUST carry, at minimum, the route path and HTTP method for
  `auth.denied` events, and the required permission set the route
  declared. Consumer features define payload schemas per
  `action_kind`. Audit payloads MUST NOT carry note contents,
  signatures, raw cosigner data, or any per-account secret state;
  payload is for action context only.
- **FR-026**: Once persisted, an `admin_actions` row MUST NOT be
  modifiable through the running server. Enforcement is a Postgres
  trigger that raises on `UPDATE` or `DELETE` (see §Design decisions);
  the `Auditor` trait additionally exposes no update/delete method,
  but the trigger is the trusted layer. Out-of-band DB superuser
  access remains out of scope. Log-fallback events are append-only by
  construction.
- **FR-027**: If the Postgres-backed audit write fails on a denial,
  the server MUST still return the denial response to the caller
  AND the writer MUST emit the same structured log line that the
  non-Postgres fallback would have emitted (so the event is never
  invisible). The operator response MUST NOT be blocked on the row
  landing.

**Session introspection endpoint**

- **FR-033**: The server MUST expose `GET /dashboard/session` returning
  the authenticated operator's identity and effective permission set
  as JSON `{ "operator_id": string, "permissions": string[] }`.
  `operator_id` MUST be the same value the principal already carries
  (`AuthenticatedOperator::operator_id`, today the commitment hex per
  FR-023). `permissions` MUST be the effective permission set read
  through the same FR-008 re-resolve path that the authorization
  middleware uses, ordered lexicographically (ASCII) per FR-017's
  determinism rule. The endpoint MUST be registered under the
  existing `/dashboard` Axum router so it inherits the session
  middleware.
- **FR-034**: The session introspection endpoint MUST require a valid
  operator session (existing `require_dashboard_session` middleware)
  but MUST NOT be gated by the authorization middleware. An operator
  whose allowlist entry has `permissions: []` MUST receive `200 OK`
  with an empty `permissions` array — not `403`. This is the only
  authenticated dashboard endpoint introduced or modified by this
  feature that does **not** carry a required permission set.
- **FR-035**: The session introspection endpoint MUST NOT write any
  `admin_actions` event, on success or on the `401` session-validation
  failure path. It is a read-only metadata endpoint; auditing it
  would flood the forensic stream with non-mutating noise. (Mutating
  endpoints retain their own audit obligations under FR-019.)
- **FR-036**: `@openzeppelin/guardian-operator-client` MUST expose a
  typed wrapper for `GET /dashboard/session` returning
  `{ operatorId: string, permissions: string[] }`. The TypeScript
  client MUST surface the `permissions` array using the same
  permission-string vocabulary the server emits (FR-004), so
  consumers can compare it directly against the exported permission
  constants (`DASHBOARD_READ`, `ACCOUNTS_PAUSE`, `POLICIES_WRITE`)
  without translation.

**Probe endpoint (testing aid)**

- **FR-028**: The server MUST expose exactly one gated probe
  endpoint whose only purpose is to exercise the authorization
  middleware end-to-end. The probe MUST declare required permission
  set `{accounts:pause}`. On success it MUST write one
  `admin_actions` event with `action_kind = probe.access`,
  `outcome = success`, and perform no other state change.
- **FR-029**: The probe endpoint MUST be gated by a Cargo feature
  (working name `authz-test-probe`) whose default is **off** in release
  builds. When gated off, the endpoint MUST not be registered at all
  and the server MUST return `404 Not Found` for that path, with no
  audit event written.

**Operator client (TypeScript)**

- **FR-030**: `@openzeppelin/guardian-operator-client` MUST extend
  the existing `DashboardErrorCode` union
  (`packages/guardian-operator-client/src/http.ts:45`) with a new
  variant for the permission-denial code, and `parseErrorBody`
  (`http.ts:78-129`) MUST populate `missing_permissions: string[]`
  for that variant.
- **FR-031**: Dashboards gate UI affordances by reading the
  authenticated operator's effective permission set from
  `GET /dashboard/session` (FR-033..FR-036) and comparing entries
  against the exported wire-string constants (`DASHBOARD_READ`,
  `ACCOUNTS_PAUSE`, `POLICIES_WRITE`) and the `OperatorPermission`
  union. The TypeScript client MUST export these constants for typed
  set-membership checks; the server remains authoritative on every
  call.
- **FR-032**: The TypeScript client MUST NOT short-circuit a request
  based on its own capability knowledge before contacting the server.
  The middleware on the server is the source of truth; client-side
  gating is for UI only and MUST NOT prevent a request from reaching
  the server.

### Contract / Transport Impact

- The error envelope used by Guardian server gains exactly one new
  pinned variant (`GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`)
  carrying `missing_permissions: string[]`. No other error variants
  are introduced or modified in this feature. The new fields are
  additive on the existing flat envelope (see §Design decisions).
- The dashboard read endpoints introduced by [`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/spec.md)
  acquire an implicit required permission of `{dashboard:read}`.
  Their request/response shapes are unchanged; the only observable
  difference is that operators with an explicit empty permission set
  now receive `403` instead of the existing payload (FR-003).
- The allowlist JSON gains the heterogeneous element shape per FR-001.
  Existing deployments using the legacy `Vec<String>` form load
  unchanged.
- One new dashboard endpoint is introduced: `GET /dashboard/session`,
  returning `{ operator_id, permissions }`. The endpoint requires a
  valid session but no specific permission (FR-033, FR-034). Its
  shape is added to the OpenAPI contract addendum
  (`contracts/dashboard-authz.openapi.yaml`).
- No gRPC mapping is added (operator surface is HTTP-only).

### Data / Lifecycle Impact

- **Schema migration**: one new table (`admin_actions`) added to the
  existing multisig-state Postgres DB via a new Diesel migration
  under `crates/server/migrations/`, with indexes
  `(operator_identity, occurred_at DESC)` and `(occurred_at DESC)`.
  No existing tables are altered.
- **Non-Postgres deployments**: the `Auditor` falls back to
  structured log output per FR-021. There is no JSONL file sink,
  no in-memory ring buffer, and no audit-disabled mode.
- **Allowlist format**: the existing JSON allowlist gains a
  heterogeneous element shape per FR-001. Pre-existing deployments
  using the legacy bare-hex-string form require **no edit** to
  remain functional (legacy-grant, FR-002).
- **Session lifecycle**: unchanged. Sessions remain identity-bound
  and in-process; permissions are read live from the allowlist on
  every authentication call (existing
  `state.rs::authenticate_session` reload behavior), so existing
  sessions transparently benefit from allowlist reload.

## Edge Cases *(mandatory)*

1. **Duplicate permissions in one entry**: see FR-006. Tolerated; not
   an authoring error.
2. **Duplicate `public_key` across entries**: see FR-007. Rejected at
   load time; the operator must edit the source.
3. **Mixed string and object entries in one document**: explicitly
   permitted by FR-001. A document can carry legacy bare-hex
   read-only operators alongside object-shape permissioned operators.
4. **Object entry missing the `permissions` field**: MUST be rejected
   as malformed. Use a bare hex string for legacy-grant; use
   `permissions: []` to deny everything explicitly.
5. **Unknown permission string**: see FR-004. The load fails with a
   deterministic error; on hot-reload, the previously loaded
   allowlist is retained for new requests (existing
   `ConfigurationError` propagation), and the failed reload surfaces
   as a request-time error per the existing code path. This is
   intentionally stricter than "ignore unknown" because unknown today
   might be a typo of a real permission tomorrow.
6. **Whitespace in permission strings**: see FR-005. Rejected, not
   trimmed.
7. **Allowlist source unreachable at startup**: existing
   [`002-operator-auth`](../002-operator-auth/spec.md) behavior
   applies (startup error). This feature adds no new failure mode at
   startup.
8. **Allowlist source unreachable at hot-reload**: today the existing
   reload path propagates `ConfigurationError` to the request that
   triggered the reload (`state.rs:374`); the previously loaded
   snapshot is **not** retained for that request. This is an existing
   gap in [`002-operator-auth`](../002-operator-auth/spec.md), not a
   contract this feature establishes. Permissions inherit whatever
   behavior the reload path has at the time this feature ships;
   addressing reload-resilience is out of scope here.
9. **Concurrent session and reload**: the existing
   `RwLock<OperatorAllowlist>` snapshot (`state.rs`) provides atomic
   reads of the permission set within one authentication call.
   Externally observable behavior is "no torn reads of the permission
   set within one request".
10. **Operator removed entirely from allowlist mid-session**: existing
    [`002-operator-auth`](../002-operator-auth/spec.md) revocation
    semantics apply. The session is invalidated. The authorization
    middleware does not modify this — a removed operator never reaches
    the middleware.
11. **Race: permission granted between session and request**: covered
    by FR-008. The request reads the current effective set; no
    re-login required.
12. **Race: permission revoked but session still valid**: covered by
    US3 / FR-008. Revocation of an individual permission does NOT
    invalidate the session; only full allowlist removal does. The
    next request requiring the revoked permission is denied.
13. **Postgres-backed audit write fails on a denial**: covered by
    FR-027. The denial returns; the writer emits the log-fallback
    event so the audit record is still surfaced.
14. **Non-Postgres deployment starts up**: covered by FR-021. Server
    emits a loud startup warning that audit is log-only; the writer
    proceeds to emit log events for every audit-worthy action.
15. **`admin_actions` writer transiently unavailable on success**
    (relevant to future mutating endpoints): consumer endpoints
    choose their own success-vs-audit ordering. This feature does not
    pin "audit-before-mutate" or "mutate-before-audit" globally; each
    consumer feature (starting with [#181](https://github.com/OpenZeppelin/guardian/issues/181))
    pins its own ordering. v1 of this feature only pins the writer
    interface and the denial-side ordering.
16. **Empty `missing_permissions` on a denial**: MUST NOT occur. If
    the middleware denies, at least one required permission was
    missing; a denial response with an empty array would indicate a
    server bug and SHOULD fail integration tests.
17. **Operator with permission for a feature that does not yet
    exist** (e.g. someone preemptively granted `policies:write`
    before [#182](https://github.com/OpenZeppelin/guardian/issues/182)
    ships): the allowlist loads (the permission is in the v1
    vocabulary), but no endpoint requires it yet, so the grant is
    inert. This is intentional — pre-provisioning is allowed.
18. **Probe endpoint hit while Cargo feature is off**: covered by
    FR-029. `404`, no audit event.
19. **Audit event for an unauthenticated caller**: MUST NOT occur. The
    middleware runs after session validation; `401` paths never reach
    the audit writer. This keeps unauthenticated noise out of the
    forensic stream.

## Success Criteria *(mandatory)*

### Measurable Outcomes

1. **SC-001**: An allowlist of bare hex strings continues to call
   every existing dashboard read endpoint successfully, with response
   payloads identical to the pre-feature contract — verified by
   replaying the existing `005-operator-dashboard-metrics`
   integration suite against an unchanged allowlist after this
   feature lands. Zero test changes required to that suite.
2. **SC-002**: An object-shape entry with `permissions: []` is denied
   with `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` on every
   dashboard read endpoint and every gated probe endpoint, with
   `missing_permissions` listing the required permission(s) in
   deterministic order.
3. **SC-003**: An object-shape entry granted `accounts:pause` can call
   the gated probe endpoint and produce exactly one `admin_actions`
   event per call with `outcome = success`, `error_code = NULL`; an
   entry without `accounts:pause` is denied and produces exactly one
   event with `outcome = denied`, `error_code =
   GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`. On Postgres
   deployments these are rows; on non-Postgres deployments they are
   log lines under the audit selector.
4. **SC-004**: A live permission grant or revocation written to the
   allowlist source takes effect on the **next authentication call**
   from an already-active session (existing reload-on-authenticate
   path), with no server restart.
5. **SC-005**: An allowlist with any unknown permission string is
   rejected on load (startup or hot-reload), and the rejection
   surfaces a deterministic error identifying the offending entry
   and the unknown permission string.
6. **SC-006**: An allowlist with a duplicate `public_key` across two
   entries is rejected on load with a deterministic error.
7. **SC-007**: Every denial response from the authorization
   middleware carries HTTP `403` and the typed code
   `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION`; no other Guardian
   error path returns this code.
8. **SC-008**: The TypeScript operator client extends the existing
   `DashboardErrorCode` union with the new variant, and exports the
   three wire-string permission constants (`dashboard:read`,
   `accounts:pause`, `policies:write`) that match the server's
   vocabulary in `crates/server/src/dashboard/permissions.rs` —
   verified by a TS test that asserts each constant matches its
   wire-string value.
9. **SC-009**: Direct `UPDATE` or `DELETE` against a persisted
   `admin_actions` row through the running server's writer surface
   fails. The exact enforcement layer (Postgres trigger vs Rust trait
   surface) is pinned during planning; SC-009 holds for whichever
   layer the plan picks.
10. **SC-010**: The gated probe endpoint returns `404` and writes no
    audit event when the Cargo feature is off, regardless of the
    calling operator's permission set or session state — verified by
    a release-build smoke test.
11. **SC-011**: On a non-Postgres deployment, every audit-worthy
    middleware event produces exactly one structured log line under
    the `audit.admin_action` selector with the same fields as the
    Postgres row — verified by an integration test that runs the
    server with the filesystem `MetadataStore` and asserts the log
    contents.
12. **SC-012**: On a Postgres deployment, a fault-injected
    transient DB write failure does not suppress the denial response
    AND produces exactly one fallback log line under the audit
    selector — verified by a fault-injection integration test.
13. **SC-013**: `GET /dashboard/session` returns the authenticated
    operator's identity and permission set in lexicographic order
    for sessions backed by non-empty allowlist entries, returns the
    identity with an empty `permissions: []` array for sessions
    backed by explicit-empty entries, and returns `401` for callers
    with no valid session — all without writing any `admin_actions`
    event. After a hot-reload that grants or revokes a permission,
    the next call within an already-active session reflects the new
    set, verified by an integration test that edits the allowlist
    source mid-session.

## Assumptions

1. **Legacy-grant default**. Existing deployed operator entries are
   bare hex strings (legacy `Vec<String>` shape) and treating them as
   `{dashboard:read}` preserves observed behavior. Any deployment
   that wants stricter defaults can edit the allowlist after this
   feature ships; this feature does not flip existing deployments
   into a denial-by-default posture.
2. **Permission vocabulary is server-defined and small**. The server
   knows exactly the strings it accepts. There is no plan in v1 for
   operators to invent permission strings. New permissions arrive
   alongside new mutating endpoints (e.g.
   [#181](https://github.com/OpenZeppelin/guardian/issues/181) is the
   first user of `accounts:pause`;
   [#182](https://github.com/OpenZeppelin/guardian/issues/182) ships
   `policies:write`'s first consumer).
3. **All three allowlist sources share one JSON schema**. The
   `GUARDIAN_OPERATOR_PUBLIC_KEYS_JSON` deploy variable, the
   `_FILE` source, and the `_SECRET_ID` payload are parsed by the
   same `from_json` entrypoint (`allowlist.rs:125`). Extending the
   schema in one place covers all three.
4. **`admin_actions` lives in the existing multisig-state Postgres
   DB** when Postgres is present; there is no separate operator-side
   database today, and v1 does not introduce one. For deployments
   running on the filesystem metadata backend, audit events flow to
   structured logs instead.
5. **No operator surface gRPC**. The operator surface is HTTP-only
   today (`crates/server/proto/guardian.proto:6-42`). This feature
   does not add gRPC and does not pin gRPC behavior.
6. **Operator identity is stable across permission changes**. The
   identity tied to an `admin_actions` event is the `operator_id`
   from `AuthenticatedOperator` (today the commitment hex); adding
   or removing permissions does not change the identity, so forensic
   queries by operator identity are stable across grants.
7. **Volume is small**. The architecture document estimates tens of
   `admin_actions` events per day per deployment. No retention work
   ships in v1 on that basis. The log-fallback path inherits whatever
   retention the existing stdout/stderr log collection provides.
8. **Sessions remain in-process**. Multi-replica deployments today
   already have per-replica sessions; this feature does not change
   that. A permission grant or revocation applies on the replica
   that handles the next request from that operator, not globally.
   Cross-replica session sharing is a separate concern.
9. **DB-backed operator storage is a follow-up**. The end-state
   design is a Postgres `operators` table plus dashboard CRUD
   endpoints. Ship that as a separate feature once #181 is unblocked
   (see §Dependencies).
10. **The structured error envelope from [#179](https://github.com/OpenZeppelin/guardian/issues/179)
    is a soft prerequisite, not a hard one**. This feature pins one
    new error code on the existing flat envelope (additively); when
    #179 lands its nested shape, this code rides it without a
    re-spec because its typed fields are envelope-independent.

## Dependencies

- [`002-operator-auth`](../002-operator-auth/spec.md) — session model,
  identity model, allowlist source(s), reload-on-authenticate path.
- [`003-operator-account-apis`](../003-operator-account-apis/spec.md),
  [`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/spec.md) —
  consumers of the new `{dashboard:read}` requirement; their tests
  are the regression bar for SC-001.
- [#179](https://github.com/OpenZeppelin/guardian/issues/179) — soft
  prerequisite: structured error envelope. This feature ships one
  code; FR-016 keeps shape flexibility.
- [#181](https://github.com/OpenZeppelin/guardian/issues/181) — first
  real consumer (`accounts:pause`). Lands in its own spec / PR after
  this feature.
- [#182](https://github.com/OpenZeppelin/guardian/issues/182) — second
  real consumer (`policies:write`). Lands in its own spec / PR after
  this feature.
- **Follow-up ticket (TBD, to be filed)** — DB-backed `operators`
  table + dashboard endpoints for operator/permission CRUD. v1 manages
  operators by editing the existing allowlist JSON source; the
  follow-up replaces that with a queryable, runtime-mutable Postgres
  table.

## Design decisions

- **Error envelope (FR-016)**: extend the existing flat
  `ErrorResponse` envelope additively. The new fields
  (`missing_permissions`, `retryable`) are serialized only for the
  permission-denial code; every other code emits the same bytes as
  before. Avoids coupling this feature to the broader nested-envelope
  migration tracked under #179.
- **Append-only enforcement (FR-026)**: Postgres trigger
  `admin_actions_no_update` raises on `UPDATE`/`DELETE`. The Rust
  `Auditor` trait additionally exposes no update/delete methods, but
  the trigger is the trusted layer — a code refactor that adds a
  cleanup path cannot silently bypass append-only. Future retention
  work must drop the trigger explicitly in its own migration.
- **Probe endpoint gating (FR-028 / FR-029)**: Cargo feature
  `authz-test-probe`, default off. Build-time gating means the
  binary's feature list is the audit surface — no runtime flag, no
  env-var toggle. CI builds with the feature; production builds
  without it return `404` for the path.
