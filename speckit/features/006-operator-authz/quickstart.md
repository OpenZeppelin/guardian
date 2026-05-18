# Quickstart: Operator Authorization Foundation

**Feature Key**: `006-operator-authz` | **Date**: 2026-05-15

Walks the happy path and the denial path against a locally-running
Guardian built with the `authz-test-probe` Cargo feature. Postgres is
optional; the log-fallback path is exercised explicitly in step 6.

## 0. Prerequisites

- Local Guardian checkout, dependencies installed
  (`cargo build -p guardian-server --features authz-test-probe`).
- Postgres optional. If running without Postgres, the
  `Auditor` falls back to structured logs (FR-021); a startup
  `WARN` line announces it.
- Two Falcon keypairs handy: `KEY_A` (read-only operator) and
  `KEY_B` (pause-capable operator). The
  `examples/operator-smoke-web` harness generates these for you.

## 1. Configure the allowlist with mixed shapes

Create `/tmp/operator-allowlist.json`:

```json
[
  "0x<hex of KEY_A>",
  {
    "public_key": "0x<hex of KEY_B>",
    "permissions": ["dashboard:read", "accounts:pause"]
  }
]
```

Start the server pointing at the file:

```bash
GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE=/tmp/operator-allowlist.json \
  cargo run -p guardian-server --features authz-test-probe
```

Expected startup behavior:

- Server logs `loaded operator allowlist (2 entries)` (or similar).
- If running without Postgres, a `WARN` line announces
  `audit events will not be persisted (filesystem backend)`.

## 2. Authenticate as the read-only operator (KEY_A)

Drive the existing `002-operator-auth` challenge/verify flow with
`KEY_A`. After verify, the response sets the
`guardian_operator_session` cookie. Equivalent shell flow (using
the operator client harness in `examples/operator-smoke-web`):

```bash
# Issue challenge
curl -s -c /tmp/jar -X GET http://localhost:8080/auth/challenge?commitment=$KEY_A_COMMITMENT

# Sign the returned payload offline with KEY_A's Falcon key
# (see 002-operator-auth quickstart for the helper script)

# Submit verify
curl -s -b /tmp/jar -c /tmp/jar -X POST http://localhost:8080/auth/verify \
  -H 'Content-Type: application/json' \
  --data '{"commitment":"...","signature":"..."}'
```

## 3. Read endpoints work for the read-only operator

```bash
curl -s -b /tmp/jar http://localhost:8080/dashboard/accounts
```

Expected: `200 OK` with the existing dashboard accounts payload —
unchanged from before this feature shipped (SC-001). The middleware
required `{dashboard:read}` and KEY_A held it via legacy-grant.

## 4. Hit the probe with the read-only operator (denial path)

```bash
curl -i -b /tmp/jar -X POST http://localhost:8080/dashboard/_authz_probe
```

Expected response:

```http
HTTP/1.1 403 Forbidden
Content-Type: application/json

{
  "success": false,
  "code": "GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION",
  "error": "Operator lacks required permissions: accounts:pause",
  "missing_permissions": ["accounts:pause"],
  "retryable": false
}
```

Inspect the audit trail. On Postgres deployments:

```sql
SELECT operator_identity, action_kind, outcome, error_code, payload
  FROM admin_actions
  ORDER BY occurred_at DESC LIMIT 1;
```

Expected one row: `(KEY_A commitment, "auth.denied", "denied",
"GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION", {route_path:
"/dashboard/_authz_probe", http_method: "POST", required_permissions:
["accounts:pause"]})`.

On filesystem-only deployments, grep the server log:

```bash
grep audit.admin_action server.log
```

Expected one matching log line with the same fields (Decision 4).

## 5. Authenticate as the pause-capable operator (KEY_B), retry probe (success path)

Repeat the challenge/verify flow from step 2 with `KEY_B`. Then:

```bash
curl -i -b /tmp/jar -X POST http://localhost:8080/dashboard/_authz_probe
```

Expected: `204 No Content`. The audit table (or log) now contains
one additional row with `action_kind = probe.access`, `outcome =
success`, `error_code = NULL`, and the same `operator_identity`
field populated from KEY_B's commitment.

## 6. Verify the log-fallback path (filesystem-only deployment)

Re-run steps 1–5 with `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` set but
**no** Postgres connection configured. The server starts with the
filesystem `MetadataStore` and the warning:

```text
WARN audit events will not be persisted (filesystem backend); structured logs only
```

Every denial and success step still emits one structured log line
under `target = audit.admin_action`. The Postgres `admin_actions`
table does not exist; querying it through Diesel from a release
build would fail. (FR-021, SC-011.)

## 7. Verify hot-reload of permissions

While KEY_A's session is still valid (step 2 cookie still in
`/tmp/jar`), edit `/tmp/operator-allowlist.json`:

```json
[
  {
    "public_key": "0x<hex of KEY_A>",
    "permissions": ["dashboard:read", "accounts:pause"]
  },
  {
    "public_key": "0x<hex of KEY_B>",
    "permissions": ["dashboard:read", "accounts:pause"]
  }
]
```

Re-issue the probe with the **same** KEY_A cookie:

```bash
curl -i -b /tmp/jar -X POST http://localhost:8080/dashboard/_authz_probe
```

Expected: `204 No Content`. The next authentication call after the
file edit re-resolved `effective_permissions` from the live
allowlist snapshot (FR-008 / SC-004); no re-login was required.

## 8. Verify duplicate-key rejection at load

Edit `/tmp/operator-allowlist.json` to include the same
`public_key` twice:

```json
[
  "0x<hex of KEY_A>",
  {
    "public_key": "0x<hex of KEY_A>",
    "permissions": []
  }
]
```

Trigger a reload (next authenticated request). Expected: the
request fails with a `ConfigurationError`, the server log contains
a deterministic error identifying the duplicate, and the previously
loaded allowlist remains in effect for subsequent requests (FR-007 /
SC-006).

## 9. Error matrix smoke checklist

Run through every row before concluding the feature works:

| Step | Operator | Endpoint | Expected status | Expected `admin_actions` |
|------|----------|----------|----------------|--------------------------|
| 3 | KEY_A (legacy hex) | `GET /dashboard/accounts` | `200` | none (success on read does not audit in v1) |
| 4 | KEY_A | `POST /dashboard/_authz_probe` | `403` | `auth.denied` |
| 5 | KEY_B (pause-capable) | `POST /dashboard/_authz_probe` | `204` | `probe.access` |
| -- | (no session) | `POST /dashboard/_authz_probe` | `401` | none (auth fails before middleware) |
| 7 | KEY_A after reload | `POST /dashboard/_authz_probe` | `204` | `probe.access` |
| -- | KEY_A with `permissions: []` | `GET /dashboard/accounts` | `403` | `auth.denied` |
| 8 | KEY_A | (any) | request fails on reload | none |
