---
name: smoke-test-operator-dashboard
description: Run or guide manual smoke testing of Guardian operator dashboard auth and account APIs through `examples/operator-smoke-web` and `@openzeppelin/guardian-operator-client`. Use when Codex needs to verify operator challenge signing, session login, logout, account listing, account detail, local signer behavior, local or remote Guardian targets, or workspace versus published operator client package behavior.
---

# Smoke Test Operator Dashboard

Use `examples/operator-smoke-web` as the primary smoke surface for operator auth and dashboard account APIs. The default path uses the browser-generated local Falcon signer, not Miden Wallet.

## Targets

| Target | Guardian | Operator client package | When to use |
| --- | --- | --- | --- |
| Local workspace | `http://127.0.0.1:3000` | `file:../../packages/guardian-operator-client` | default for in-repo changes |
| Remote Guardian | deployed HTTP endpoint | `file:../../packages/guardian-operator-client` | verify a deployed server with current local client |
| Published client | local or remote Guardian | npm package version under test | verify a future published `@openzeppelin/guardian-operator-client` |

## Preflight

1. Check the current implementation before assuming UI labels or response shapes:
   - `examples/operator-smoke-web/src/App.tsx`
   - `examples/operator-smoke-web/src/localSigner.ts`
   - `packages/guardian-operator-client/src/http.ts`
   - `crates/server/src/api/dashboard.rs`
   - `crates/server/src/dashboard/mod.rs`
2. Run the focused checks:
   ```bash
   cd packages/guardian-operator-client && npm run typecheck && npm test && npm run build
   cd examples/operator-smoke-web && npm run typecheck && npm run build
   cargo test -p guardian-server api::dashboard::tests
   cargo test -p guardian-server dashboard::tests
   ```
3. Confirm no unrelated dirty files are needed for the smoke. Runtime state should live outside the repo unless the user explicitly asks otherwise.

## Local Guardian

Use a file-backed allowlist so the operator commitment can be changed without restarting the server.

```bash
mkdir -p /tmp/guardian-operator-smoke
printf '[]\n' > /tmp/guardian-operator-smoke/operator-allowlist.json
GUARDIAN_OPERATOR_ALLOWLIST_PATH=/tmp/guardian-operator-smoke/operator-allowlist.json \
GUARDIAN_DASHBOARD_ALLOW_INSECURE_HTTP=true \
GUARDIAN_DASHBOARD_DOMAIN='*' \
cargo run -p guardian-server --bin server
```

If port `3000` is already occupied, inspect the process and reuse it only if it was started with `GUARDIAN_OPERATOR_ALLOWLIST_PATH`. Otherwise stop it or use a different server port and set `VITE_GUARDIAN_TARGET`.

## Operator UI

For workspace-source smoke:

```bash
cd examples/operator-smoke-web
npm install
VITE_GUARDIAN_TARGET=http://127.0.0.1:3000 npm run dev -- --host 127.0.0.1 --port 3003
```

For remote Guardian smoke, keep the same UI and change only `VITE_GUARDIAN_TARGET`.

For published-client smoke, do not mutate the committed example dependency. Create a scratch copy outside the repo, replace `@openzeppelin/guardian-operator-client` with the published version, run `npm install`, and record `npm ls @openzeppelin/guardian-operator-client`.

## Automated Browser Run

Use the bundled script when a real browser automation pass is requested. It drives the local signer UI, writes the generated allowlist entry, logs in, lists accounts, fetches the first account detail when present, logs out, and confirms a protected request fails afterward.

Install the automation dependency into local runtime state:

```bash
mkdir -p /tmp/guardian-operator-smoke-playwright
npm install --prefix /tmp/guardian-operator-smoke-playwright playwright-core
```

Run from the repo root after Guardian and the UI are listening:

```bash
GUARDIAN_URL=http://127.0.0.1:3000 \
GUARDIAN_OPERATOR_SMOKE_URL=http://127.0.0.1:3003/ \
GUARDIAN_OPERATOR_ALLOWLIST_PATH=/tmp/guardian-operator-smoke/operator-allowlist.json \
PLAYWRIGHT_CORE_INSTALL_ROOT=/tmp/guardian-operator-smoke-playwright \
node .agents/skills/smoke-test-operator-dashboard/scripts/run-operator-smoke.mjs
```

Set `HEADLESS=false` to watch the browser. Set `CHROME_EXECUTABLE` if Chrome is not at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`.

## Manual Flow

1. Open `http://127.0.0.1:3003/`.
2. Click `Generate local Falcon signer`.
3. Put the UI's `Allowlist JSON` into the active operator allowlist file.
4. Confirm `GET /auth/challenge` succeeds for the displayed commitment:
   ```bash
   curl -sS -G "$GUARDIAN_URL/auth/challenge" --data-urlencode "commitment=$COMMITMENT"
   ```
5. Click `Login`.
6. Click `List accounts`.
7. If any accounts are returned, fetch one detail by pasting its account ID and clicking `Fetch account`.
8. Click `Logout`, then verify a protected action such as `List accounts` is rejected until logging in again.

Use `GUARDIAN_URL=http://127.0.0.1:3000` for local runs. For deployed runs, use the deployed Guardian URL.

## Assertions

- The allowlist commitment exactly matches the active local signer commitment in the UI.
- `Request challenge` returns `success: true` and a `signing_digest`.
- `Login` returns `operatorId` and `expiresAt`, and the browser receives a session cookie.
- `List accounts` returns `success: true`, `totalCount`, and an `accounts` array.
- `Fetch account` returns account detail for an existing account or a clear `404` for a missing account.
- `Logout` invalidates the session; protected dashboard requests should fail afterward.

## Failure Triage

- `Invalid operator credentials`: first check the server was started with the same allowlist file that was edited, then compare the UI commitment with the file.
- Challenge succeeds but login fails: verify the UI is using the same generated signer whose commitment was allowlisted.
- `401` on account routes after login: check same-origin proxying through `/guardian` and `credentials: include`.
- Empty account list is not a failure by itself; fetch detail only when the list returns an account ID.
- A server restart clears in-memory sessions; log in again after restart.

## Report

Report:

- Guardian target and whether it was local or remote
- operator client source: workspace path or published npm version
- commands run
- allowlisted operator ID and commitment
- login result and session expiry
- account list count
- account detail result, or skipped because the list was empty
- logout result
- every error observed, including recovered errors
- checks that passed and checks skipped with reason
