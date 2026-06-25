# Operator client scripts

Example Node scripts that drive the Guardian operator dashboard API with
[`@openzeppelin/guardian-operator-client`](../../../../packages/guardian-operator-client).
Use them to script monitoring or pause/unpause automation, or just to see the
operator flow without the dashboard UI.

All scripts share [`session.ts`](./session.ts) — its `connect()` does the SDK/WASM
init, key load, cookie jar, and challenge→sign→verify login, and returns an
authenticated client. So each operation script below is just a few lines.

| Script | What it does |
|---|---|
| `generate-operator-key.ts` | Generate a Falcon operator keypair — prints the private key, commitment, and a ready-to-paste `operators.json` entry. (No login; standalone.) |
| `session.ts` | Shared helper: `connect()` → authenticated `GuardianOperatorHttpClient`. Imported by the rest. |
| `list-accounts.ts` | List all accounts with state / pause badge. |
| `list-deltas.ts` | List deltas — `[accountId]` for one account, or the global feed. |
| `list-proposals.ts` | List in-flight proposals with signature progress — `[accountId]` or global. |
| `pause.ts` | `<accountId> <reason>` — pause an account. |
| `unpause.ts` | `<accountId> [reason]` — unpause an account. |
| `operator-demo.ts` | End-to-end walkthrough (login → permissions → list → pause → unpause) in one run. |

```bash
npm install

# generate a key (no server needed)
npx tsx generate-operator-key.ts

# then, against a running Guardian, with the key exported:
export GUARDIAN_URL=http://localhost:3000
export GUARDIAN_OPERATOR_PRIVATE_KEY=<hex from generate-operator-key.ts>
npx tsx list-accounts.ts
npx tsx list-deltas.ts            # or: list-deltas.ts <accountId>
npx tsx list-proposals.ts
npx tsx pause.ts <accountId> "compliance hold"
npx tsx unpause.ts <accountId>
npm run demo                      # the full walkthrough (operator-demo.ts)
```

`generate-operator-key.ts` is adapted from the dashboard repo's
[`scripts/generate-operator-key.ts`](https://github.com/0xMiden/guardian-dashboard/blob/main/scripts/generate-operator-key.ts)
to run standalone here (the Miden SDK is already a dependency), with the `.wasm`
resolved relative to the script rather than the working directory. The private
key + commitment go into the dashboard's `GUARDIAN_ENDPOINTS` entry (or
`GUARDIAN_OPERATOR_PRIVATE_KEY` for `operator-demo.ts`); the public key goes into
`operators.json`.

The operator's **public** key must be in the server's `operators.json`. Listing
needs `dashboard:read`; pause/unpause needs `accounts:pause`. Point `GUARDIAN_URL`
at a running Guardian — e.g. the stack from the [parent guide](../README.md),
where the server is published on `localhost:3000`.

## Two Node gotchas `session.ts` handles

Both bite if you lift a browser snippet straight into Node:

- **SDK init** — the bare `@miden-sdk/miden-sdk` import resolves to a NAPI build
  whose `AuthSecretKey.deserialize` throws in Node. `session.ts` uses the `/lazy`
  WASM build with explicit `initSync` (same as the dashboard's key script), and
  loads the `.wasm` by filesystem path because the package's `exports` map
  doesn't expose that asset to `require.resolve`.
- **Cookies** — the operator session is a cookie. Browser `fetch` keeps a cookie
  jar automatically; Node `fetch` does not, so `session.ts` injects a tiny jar
  via the client's `fetch` option. Without it, every call after `verify` would
  look unauthenticated.

Pinned to `@miden-sdk/miden-sdk@0.15.1` and
`@openzeppelin/guardian-operator-client@0.15.0`.
