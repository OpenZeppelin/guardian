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
7. Use `List accounts`, `Fetch account`, and `Logout`.

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

| Profile | Dashboard reads | Probe (`POST /dashboard/_authz_probe`)\* |
|---------|-----------------|--------------------------|
| A — read-only | `200` | `403` + `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` |
| B — pause-capable | `200` | `204` |
| C — explicitly denied | `403` on every read | `403` |

\*The probe endpoint is gated by the `authz-probe` Cargo feature. Start
Guardian with `cargo run -p guardian-server --features authz-probe`
when smoke-testing US2; release builds return `404` for that path.

The browser UI's `Operator Public Keys JSON` field shows the legacy
single-string form — to test profile B or C, paste a JSON array of
mixed entries into the file directly and the next Guardian reload
will pick them up (hot-reload is already supported by
`002-operator-auth`).
