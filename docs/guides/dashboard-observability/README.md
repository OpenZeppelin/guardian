# Dashboard UI + live observability, in one stack

Stand up a local Guardian server, the operator dashboard UI
([`0xMiden/guardian-dashboard`](https://github.com/0xMiden/guardian-dashboard)),
**and** a Prometheus/Grafana observability stack with one `docker compose up` —
log in as an operator, browse accounts, and watch the server's metrics live.

This is the assembly of two existing guides; it does not re-explain them:

- [`../miden-dashboard`](../miden-dashboard/README.md) — the dashboard UI, the
  challenge→session auth flow, the operator allowlist, and Clerk setup.
- [`../observability`](../observability/README.md) — the metrics catalogue, the
  Grafana dashboard panels, and the Prometheus scrape model.

For the dashboard trust model and permission vocabulary see
[`DASHBOARD.md`](../../DASHBOARD.md); for any server variable,
[`CONFIGURATION.md`](../../CONFIGURATION.md).

## How it fits together

Same server, two consumers. The browser only talks to the dashboard; the
dashboard's **Next.js backend** holds the operator's Falcon private key and signs
the challenge→session flow against the server at `http://server:3000` (so no CORS
config is needed). In parallel, the server exposes Prometheus metrics on `:9464`
(internal only), Prometheus scrapes it, and Grafana renders the pre-built Guardian
dashboard. The server uses the **published postgres image** — metrics is a runtime
toggle, not a build feature, so no source build is required.

The Prometheus config and the Grafana dashboard are **reused directly** from
[`../observability`](../observability) via relative paths in the compose file, so
that directory must be present (it is, in a normal checkout) — there is no copy to
drift out of sync.

| Port (loopback) | Service |
|---|---|
| `3000` | server HTTP (`curl /pubkey`) |
| `50051` | server gRPC |
| `3001` | dashboard UI |
| `3002` | Grafana |
| `9090` | Prometheus |
| `9464` | metrics — **not** published; Prometheus reaches it on the internal network |

## Prerequisites

- Docker.
- A clone of the dashboard inside this guide directory:
  ```bash
  git clone https://github.com/0xMiden/guardian-dashboard
  ```
  Override the location with `GUARDIAN_DASHBOARD_PATH` in `.env`.
- A free [Clerk](https://dashboard.clerk.com) application — the dashboard uses
  Clerk for human sign-in and cannot start without its keys.

## 1. Generate an operator key

From your `guardian-dashboard` clone:

```bash
npm install
npx tsx scripts/generate-operator-key.ts
```

It prints a `GUARDIAN_OPERATOR_PRIVATE_KEY` / `GUARDIAN_OPERATOR_COMMITMENT` pair
and a `["0x…"]` public key. In step 3 the private key and commitment go into the
`privateKey` / `commitment` fields of the `GUARDIAN_ENDPOINTS` entry, and the
public key goes into `operators.json`. Keep the private key secret. (Keys from
the multisig SDK or the
[`operator-smoke-web`](../../../examples/operator-smoke-web) UI work too, as long
as you can export all three values.)

## 2. Configure Clerk

Copy the **test** publishable and secret keys for step 3. Create (or reuse) the
admin user and set its **public metadata** so the dashboard authorises it:

```json
{ "role": "admin", "endpointIds": ["local"] }
```

`endpointIds` must include the endpoint `id` from `GUARDIAN_ENDPOINTS` (`local`
here), or sign-in succeeds but shows no nodes.

## 3. Configure the environment

```bash
cp .env.example .env
cp operators.example.json operators.json
```

In `.env` set:

- `POSTGRES_PASSWORD` — a strong, stable, URL-safe value for the bundled Postgres.
- `NEXT_PUBLIC_CLERK_PUBLISHABLE_KEY` / `CLERK_SECRET_KEY` — the test keys from step 2.
- `GUARDIAN_ENDPOINTS` — fill `commitment` and `privateKey` from step 1; leave
  `url` as `http://server:3000` and keep `network` equal to `GUARDIAN_NETWORK_TYPE`.

In `operators.json` replace `0x<your-falcon-operator-pubkey>` with the public key
from step 1. The default entry grants `dashboard:read` and `accounts:pause`; the
server hot-reloads the file on every request, so you can edit operators without a
restart.

Metrics need no configuration — the server has them enabled in the compose file
with the throwaway `devtoken`, matching the scrape config reused from
`../observability`.

## 4. Run

```bash
docker compose up
```

The first dashboard boot runs `npm install` inside the container and is slow;
later boots reuse the cached `node_modules` volume.

## 5. Validate

Server is live and serving ACK keys:

```bash
curl -s localhost:3000/pubkey | jq .
```

- **Dashboard** — open <http://localhost:3001>, sign in through Clerk, select the
  **Local Guardian** endpoint, and confirm the account list loads. That proves
  Clerk sign-in → operator challenge→session → an authenticated
  `/dashboard/accounts` call.
- **Grafana** — open <http://localhost:3002> and land straight on the Guardian
  dashboard (anonymous admin, dev only). Panels populate within a scrape interval
  or two as you exercise the server.
- **Prometheus** — <http://localhost:9090>; check **Status → Targets** shows the
  `guardian` target `UP`.

## Troubleshooting

This stack inherits both guides' failure modes — see
[`../miden-dashboard`](../miden-dashboard/README.md#troubleshooting) and
[`../observability`](../observability/README.md) first. Combination-specific:

| Symptom | Likely cause |
|---|---|
| `server` fails to start: `mounting ".../operators.json" ... not a directory` | You ran `docker compose up` before creating `operators.json`, so Docker auto-created it as a **directory**. Fix the host file (`rm -rf operators.json`, then `cp operators.example.json operators.json` and add your operator public key), then **recreate** the container — the bad mount is baked into the existing one: `docker compose down && docker compose up` (plain `down`, not `-v`, to keep your Postgres/keystore volumes). |
| Grafana / Prometheus container fails to mount its config | This guide reads `../observability/...` by relative path — run from this directory, with the `observability` guide present in the checkout. |
| Port 3001 or 3002 already in use | Stop the conflicting process, or remap the published port in `docker-compose.yml`. |
| Prometheus target `DOWN` | The server isn't up yet, or `GUARDIAN_METRICS_BEARER_TOKEN` was changed away from `devtoken` without updating `../observability/prometheus/prometheus.yml`. |

See [`TROUBLESHOOTING.md`](../../TROUBLESHOOTING.md) for the full server
error-code playbook.
