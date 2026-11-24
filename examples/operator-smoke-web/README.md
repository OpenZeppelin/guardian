# `examples/operator-smoke-web`

Minimal browser UI for Guardian operator endpoints using a generated Falcon key.

This example does only these things:

- generate and persist a Falcon key in the browser
- show the derived public key and commitment
- call `@openzeppelin/guardian-operator-client`
- let you manually drive challenge, login, account list, account detail, and logout
- read and write policy module state (see [Policy modules panel](#policy-modules-panel))

## Setup

Install the shared TypeScript workspace dependencies once from the repository
root. The example's `dev`, `build`, and `typecheck` commands rebuild the local
operator client automatically. Then start the example so it can generate the
local Falcon signer public key:

```bash
cd packages
npm ci

cd ../examples/operator-smoke-web
npm ci
npm run typecheck
npm run dev -- --host 127.0.0.1 --port 3003
```

Create a local operator public keys file and start Guardian with that path:

```bash
mkdir -p /tmp/guardian-operator-smoke
printf '[]\n' > /tmp/guardian-operator-smoke/operator-public-keys.json

GUARDIAN_NETWORK_TYPE=MidenLocal \
GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE=/tmp/guardian-operator-smoke/operator-public-keys.json \
cargo run -p guardian-server --bin server
```

By default the UI uses `/guardian` as the base URL and the Vite dev proxy
forwards it to `http://127.0.0.1:3000`.

To watch Prometheus metrics move while exercising the smoke flow, add
`GUARDIAN_METRICS_ENABLED=true` (and optionally
`GUARDIAN_METRICS_REFRESH_INTERVAL_SECS=5`) to the `cargo run`
invocation above, then:

```bash
curl -s http://127.0.0.1:9464/metrics | grep -E 'guardian_(operator_auth|http_requests_total)'
```

Login attempts show up under `guardian_operator_auth_*` and every
dashboard call under `guardian_http_requests_total` (labelled by route
template). See `docs/CONFIGURATION.md` ("Runtime — metrics") for the
full configuration surface.

To point at a different Guardian target:

```bash
VITE_GUARDIAN_TARGET=https://your-guardian.example npm run dev
```

## Manual Flow

1. Open `http://127.0.0.1:3003/`.
2. Click `Generate local Falcon signer`.
3. Copy the `Operator Public Keys JSON` value from the UI into the configured JSON file.
4. Keep Guardian running with `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` pointed at that file.
5. Click `Request challenge`.
6. Click `Login`.
7. Use `List accounts`, `Fetch account`, `Get session`, and `Logout`.
8. Use `Dashboard stats` / `Dashboard stats (7d)` for the one-call account and
   asset aggregate (`GET /dashboard/stats`, issue #371). Right after Guardian
   starts the call returns `503 data_unavailable` until the first walk is
   published (the lease holder walks every
   `GUARDIAN_DASHBOARD_STATS_REFRESH_INTERVAL_SECS`, 300 s by default; set it
   to `5` for a quicker smoke loop). `Dashboard stats (invalid since)`
   exercises the `400 invalid_timestamp` path. Compare `accounts.by_auth_method`
   and `as_of` with `accounts_by_auth_method` / `aggregates_as_of` from
   `Dashboard info` — both read the same published snapshot and must agree.
9. `Request stats refresh` calls `POST /dashboard/stats/refresh`, which needs
   the `stats:refresh` permission on the allowlist entry (see the profiles
   below): expect `202` with `status: "queued"`, then `429 rate_limit_exceeded`
   for a second request inside the 60-second cooldown, and a newer `as_of` on
   the next `Dashboard stats` call once the walk has published.

`Get session` calls `GET /dashboard/session` (feature `006-operator-authz`
US6) and shows the operator's identity and effective permission set as
the server resolved them on this request. Permissions are live-read from
the allowlist, so editing the JSON file and clicking the button again
reflects the change without re-login. The endpoint requires only a
valid session — operators with `permissions: []` get `200` with an
empty array, not `403`.

## Important Note

The `Operator commitment` field is editable, but by default it is seeded from
the generated Falcon key's real `publicKey.toCommitment()` value. This avoids
the Miden Wallet browser-bridge issues and gives a stable local smoke path for
the operator client.

## Authorization profiles (feature `006-operator-authz`)

The operator allowlist JSON now accepts a heterogeneous array: each
entry is either a bare hex string (legacy, `{dashboard:read}` only) or
a structured object with explicit permissions. Three smoke profiles
help you exercise the new authorization middleware:

```jsonc
// /tmp/guardian-operator-smoke/operator-public-keys.json
[
  // Profile A — read-only operator (legacy form).
  "0x<hex of READ_ONLY signer>",

  // Profile B — read + pause capable.
  {
    "public_key": "0x<hex of PAUSE_CAPABLE signer>",
    "permissions": ["dashboard:read", "accounts:pause", "stats:refresh"]
  },

  // Profile C — explicitly denied (different from "absent").
  {
    "public_key": "0x<hex of DENIED signer>",
    "permissions": []
  }
]
```

Then exercise each profile:

| Profile | Dashboard reads | `Get session` | Probe (`POST /dashboard/_authz_probe`)\* |
|---------|-----------------|---------------|--------------------------|
| A — read-only | `200` | `200`, `permissions: ["dashboard:read"]` | `403` + `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` |
| B — pause-capable | `200` | `200`, `permissions: ["accounts:pause", "dashboard:read", "stats:refresh"]` | `204` |
| C — explicitly denied | `403` on every read | `200`, `permissions: []` (NOT `403`) | `403` |

\*The probe endpoint is gated by the `authz-test-probe` Cargo feature. Start
Guardian with `cargo run -p guardian-server --features authz-test-probe`
when smoke-testing US2; release builds return `404` for that path.

**Note on module permissions**: `policies:write`, `policies:write_global`
and `modules:disable` gate the policy module writes (see
[Policy modules panel](#policy-modules-panel)). Rolling back to a server
build that predates one of them fails to load the allowlist, so remove
them before downgrading the server binary.

The browser UI's `Operator Public Keys JSON` field shows the legacy
single-string form — to test profile B or C, paste a JSON array of
mixed entries into the file directly and the next Guardian reload
will pick them up (hot-reload is already supported by
`002-operator-auth`).

## Policy modules panel

The `Policy modules` panel drives the operator module endpoints (RFC 0002,
issue #251; not in a release yet). Endpoint semantics are in
[`docs/DASHBOARD.md`](../../docs/DASHBOARD.md#policy-modules) and the
walkthrough is the
[policy modules guide](../../docs/guides/policy-modules/README.md). The panel
covers:

- **Catalog**: `Load catalog` lists every instance with its id, kind, scope,
  hooks, enabled flag, circuit, enabled and params drift, plus the
  `catalogHash`, `paramsVersion` and undeclared instances. Each card has
  `Enable` and `Disable` (a `409 GUARDIAN_MODULE_NOT_DISABLEABLE` is expected
  for an instance without `operator_can_disable`) and `Use in forms`, which
  copies its id and current global params into the forms below.
- **Params history**: newest first for one module id. `Load older` pages with
  `before` set to the last entry's `id`; it is disabled once a page returns
  fewer entries than the limit.
- **Account modules**: `Load account modules` shows `accountParamsVersion` and
  the entries in evaluation order for the `Account ID` from the Accounts panel.
- **Writes**: set an instance's global params, set or clear an account layer
  on a `scope = all` instance, and replace the account's assignment with a
  JSON array of `{ "moduleId": ..., "params": {...} }` (`Copy current
  assignment` fills it from the loaded account modules). Params are checked
  as JSON objects in the browser before anything is sent, and every write
  sends the panel's `Reason`, which is required.

Errors show in the Session panel's `Action error` box as
`HTTP <status> [<code>] <message>`, so a missing permission reads as
`HTTP 403 [insufficient_operator_permission] ...`.

### Starting Guardian with a catalog

Use the guide's built-in catalog,
[`docs/guides/policy-modules/built-in.toml`](../../docs/guides/policy-modules/built-in.toml),
and its environment file, from the repository root (guide steps 2 and 3):

```bash
mkdir -p .guardian
cp docs/guides/policy-modules/built-in.toml .guardian/
cp docs/guides/policy-modules/.env.example docs/guides/policy-modules/.env
cp docs/guides/policy-modules/operators.example.json .guardian/operators.json
# Replace 0xREPLACE_WITH_FAUCET_ID in .guardian/built-in.toml (the server does not start until you do).

set -a && . docs/guides/policy-modules/.env && set +a
cargo run -p guardian-server --bin server
```

The guide's `.env` points `GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE` at
`.guardian/operators.json`. Instead of the guide's `modules_admin keygen`
key, paste this UI's signer public key (the string inside `Operator Public
Keys JSON`) into the `public_key` field, keeping the structured entry so the
permissions apply:

```json
[
  {
    "public_key": "0x<public key from the UI>",
    "permissions": ["dashboard:read", "policies:write", "policies:write_global", "modules:disable"]
  }
]
```

| Panel action | Permission |
|---|---|
| Load catalog, params history, account modules | `dashboard:read` |
| Set or clear an account layer, replace assignment | `policies:write` |
| Set global params, enable | `policies:write_global` |
| Disable | `policies:write_global` or `modules:disable` |

To see the `403` path, drop a permission from the entry and repeat the
write; the allowlist is reread without a restart. With `built-in.toml`, try
`Set account layer` on `blocklist` with `{"denylist": ["0x..."]}`, and
replace an account's assignment with
`[{"moduleId": "treasury-allowlist", "params": {"allowlist": ["0x..."]}}]`.
Each write then appears in `Params history` for that module.

## Inspecting audit events

Every probe denial or success writes one `admin_actions` event. The
endpoint and the underlying writer don't change shape between
backends — only the storage does. On a Postgres-backed Guardian:

```bash
docker compose -f docker-compose.postgres.yml exec postgres \
  psql -U guardian -d guardian -c \
  "SELECT id, occurred_at, operator_identity, action_kind, outcome,
          error_code, client_ip, payload
     FROM admin_actions ORDER BY occurred_at DESC LIMIT 10;"
```

On a filesystem-only Guardian the writer falls back to structured
logs under `target = audit.admin_action`:

```bash
grep audit.admin_action server.log
```

`GET /dashboard/session` calls are intentionally **not** audited
(FR-035) — only mutating attempts and authz denials produce rows.

## Verifying the append-only trigger

```bash
docker compose -f docker-compose.postgres.yml exec postgres \
  psql -U guardian -d guardian -c \
  "UPDATE admin_actions SET outcome='success' WHERE id=1;"
# → ERROR:  admin_actions is append-only
```

The trigger is the load-bearing enforcement layer (see
[spec §Design decisions](../../speckit/features/006-operator-authz/spec.md#design-decisions)).
A future retention migration is the only legitimate way to remove it.
