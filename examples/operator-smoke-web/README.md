# `examples/operator-smoke-web`

Minimal browser UI for Guardian operator endpoints using a generated Falcon key.

This example does only four things:

- generate and persist a Falcon key in the browser
- show the derived public key and commitment
- call `@openzeppelin/guardian-operator-client`
- let you manually drive challenge, login, account list, account detail, and logout

## Setup

Run Guardian first, ideally with a same-origin-friendly proxy target:

```bash
GUARDIAN_DASHBOARD_ALLOW_INSECURE_HTTP=true \
GUARDIAN_DASHBOARD_DOMAIN='*' \
cargo run -p guardian-server --bin server
```

Then start the example:

```bash
cd /Users/marcos/repos/guardian/examples/operator-smoke-web
npm install
npm run typecheck
npm run dev -- --host 127.0.0.1 --port 3003
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
3. Copy the allowlist JSON from the UI and put it into Guardian's operator
   allowlist.
4. Click `Request challenge`.
5. Click `Login`.
6. Use `List accounts`, `Fetch account`, and `Logout`.

## Important Note

The `Operator commitment` field is editable, but by default it is seeded from
the generated Falcon key's real `publicKey.toCommitment()` value. This avoids
the Miden Wallet browser-bridge issues and gives a stable local smoke path for
the operator client.
