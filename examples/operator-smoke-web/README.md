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
