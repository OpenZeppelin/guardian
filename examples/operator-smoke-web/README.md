# `examples/operator-smoke-web`

Minimal browser UI for Guardian operator endpoints using a generated Falcon key.

This example does only four things:

- generate and persist a Falcon key in the browser
- show the derived public key and commitment
- call `@openzeppelin/guardian-operator-client`
- let you manually drive challenge, login, account list, account detail, and logout

## Setup

Start the example first so it can generate the local Falcon signer public key:

```bash
cd /Users/marcos/repos/guardian/examples/operator-smoke-web
npm install
npm run typecheck
npm run dev -- --host 127.0.0.1 --port 3003
```

Create a local operator public keys file and start Guardian with that path:

```bash
mkdir -p /tmp/guardian-operator-smoke
printf '[]\n' > /tmp/guardian-operator-smoke/operator-public-keys.json

GUARDIAN_OPERATOR_PUBLIC_KEYS_FILE=/tmp/guardian-operator-smoke/operator-public-keys.json \
cargo run -p guardian-server --bin server
```

By default the UI uses `/guardian` as the base URL and the Vite dev proxy
forwards it to `http://127.0.0.1:3000`.

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
    "permissions": ["dashboard:read", "accounts:pause"]
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
| B — pause-capable | `200` | `200`, `permissions: ["accounts:pause", "dashboard:read"]` | `204` |
| C — explicitly denied | `403` on every read | `200`, `permissions: []` (NOT `403`) | `403` |

\*The probe endpoint is gated by the `authz-test-probe` Cargo feature. Start
Guardian with `cargo run -p guardian-server --features authz-test-probe`
when smoke-testing US2; release builds return `404` for that path.

**Note on `policies:write`**: the v1 permission vocabulary includes
`policies:write` but no route requires it yet. Granting it on an
allowlist entry loads cleanly but has no behavioral effect — the
first consumer ships with #182. The grant is **not** inert against
downgrade though: rolling back to a server build that predates the
permission will fail to load the allowlist. Remove forward-defined
permissions before downgrading the server binary.

The browser UI's `Operator Public Keys JSON` field shows the legacy
single-string form — to test profile B or C, paste a JSON array of
mixed entries into the file directly and the next Guardian reload
will pick them up (hot-reload is already supported by
`002-operator-auth`).

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
