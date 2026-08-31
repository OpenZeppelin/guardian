# Observability with Prometheus + Grafana

Run a Guardian server with Prometheus metrics enabled, scraped by Prometheus
and visualized in a pre-provisioned **Guardian — Server Overview** Grafana
dashboard — all from one `docker compose up`. Use it to see Guardian's metric
surface, or as a starting point for your own dashboards.

This is a local, single-host stack (filesystem storage, anonymous Grafana). It
is distinct from a production setup, where you would scrape your real server
from an existing Prometheus and protect the endpoint as below. Every setting is
in [`.env.example`](./.env.example) and [`docker-compose.yml`](./docker-compose.yml)
in this directory; for the authoritative meaning of any variable, see
[`CONFIGURATION.md`](../../CONFIGURATION.md) (“Runtime — metrics”).

## What you get

The stack runs three services on the internal Compose network:

- **server** — Guardian with `GUARDIAN_METRICS_ENABLED=true`, serving the
  Prometheus exposition on a dedicated listener (`:9464`, not published to the
  host).
- **prometheus** — scrapes `server:9464` every 15s with a bearer token.
- **grafana** — auto-provisioned with the Prometheus datasource and the
  dashboard, anonymous access for zero-friction local viewing.

The server instruments, end to end: HTTP and gRPC request paths (rate, latency,
in-flight, status/code), Miden RPC (the upstream chain node), storage
operations and per-pool (`storage`/`metadata`) DB-pool saturation,
canonicalization, the delta & proposal lifecycle, account growth, operator auth,
rate limiting, refresher health, and process metrics.

Two properties worth knowing as an operator:

- **Scrapes are cheap.** Expensive cross-account aggregates (delta counts,
  in-flight proposals, account totals, pool status) are computed by a background
  refresher every `GUARDIAN_METRICS_REFRESH_INTERVAL_SECS` and published as
  gauges — a scrape never touches the database. Staleness is observable as
  `time() - guardian_metrics_refresh_timestamp_seconds`.
- **Cardinality is bounded by construction.** Every label value comes from a
  closed set (route templates, a gRPC method allowlist, small enums). No account
  IDs, nonces, keys, IPs, or error strings become labels.

## Prerequisites

- Docker (with Compose) and outbound internet — the server connects to its Miden
  network's RPC at startup.

## 1. Start the stack

```bash
cd docs/guides/observability
cp .env.example .env     # optional — defaults work as-is
docker compose up
```

The first run builds the Guardian server image from this repo — several minutes
on a cold build, since it compiles the server in release mode (it must include
the current metrics code, so it can't use the published Postgres-only image).
Later runs reuse the cached image. Prometheus and Grafana are pulled as images.

## 2. Open the dashboard

- **Grafana** → <http://localhost:3001> — anonymous access is enabled, so you
  land directly on **Dashboards → Guardian → Guardian — Server Overview**.
- **Prometheus** → <http://localhost:9090> — check **Status → Targets**: the
  `guardian` target should be `UP`.

## 3. Generate traffic

The panels move once the server sees requests:

```bash
curl -s http://127.0.0.1:3000/pubkey >/dev/null
curl -s "http://127.0.0.1:3000/auth/challenge?commitment=0xdeadbeef" >/dev/null
```

## 4. Tear down

```bash
docker compose down          # add -v to also drop the volumes
```

## The dashboard

Panels are grouped by subsystem and map 1:1 onto the metric taxonomy in
[`spec/api.md`](../../../spec/api.md) (“Metrics Endpoint”):

| Section | Covers |
|---|---|
| Overview | build info, account count, in-flight proposals, refresh staleness, HTTP rate & error % |
| HTTP / gRPC request path | rate by route/method, status/code breakdown, p50/p95/p99 latency, in-flight |
| Miden RPC | upstream chain-node call rate, errors, p95 latency |
| Storage & DB pools | operation rate/latency, per-pool (`storage`/`metadata`) connection saturation |
| Canonicalization | run rate & duration, candidate outcomes, retries |
| Delta & proposal lifecycle | submissions, proposal events, deltas-by-status, in-flight |
| Accounts | total + creation rate by network kind |
| Auth & rate limiting | operator auth outcomes, sessions, rate-limit rejections, refresh failures |
| Process / runtime | CPU, RSS, file descriptors |

An `Instance` variable (top-left) filters to a single replica or aggregates
across all of them. Some panels stay empty in this local filesystem stack —
**DB pool** needs a Postgres build, and **Miden RPC / deltas / proposals /
account creation** need a real signed multisig flow. Copy
[`grafana/dashboards/guardian.json`](./grafana/dashboards/guardian.json) into
your own Grafana and adapt it.

## Protecting the endpoint (production)

This stack keeps the metrics listener on the internal network and gates it with
a throwaway `devtoken`. In production, defense is layered (see
[`spec/api.md`](../../../spec/api.md) and the
[security note in Configuration](../../CONFIGURATION.md#runtime--metrics-prometheus)):

1. **Network isolation first** — the listener binds loopback by default; keep
   `9464` reachable only from the scraper's network (private subnet / security
   group / sidecar).
2. **Bearer token second** — `GUARDIAN_METRICS_BEARER_TOKEN` gates scrapes with
   a constant-time check (`401` otherwise). Point Prometheus at a mounted secret
   with `authorization.credentials_file:` rather than an inline value.
3. **TLS** — terminate at a reverse proxy or sidecar where transport encryption
   is required.

Never expose `/metrics`, Prometheus, or this Grafana to a public network — the
ports here are bound to `127.0.0.1` and Grafana runs anonymous-admin precisely
because it is local-only.

## AWS deployment

The Terraform reference deployment ships this metric surface to **CloudWatch**
instead of Prometheus/Grafana: an ADOT Collector sidecar in the ECS task scrapes
the loopback-only endpoint and exports selected metrics via EMF, with a
Terraform-managed dashboard and alarms. See
[`SERVER_AWS_DEPLOY.md`](../../SERVER_AWS_DEPLOY.md#metrics-dashboard-and-alarms).

## Out of scope

Alert and recording rules and runbooks are not shipped here. The
[metric taxonomy in `spec/api.md`](../../../spec/api.md) is the reference for
writing your own — e.g. alert on `guardian_db_pool_pending_acquires` sustained
above zero, on `time() - guardian_metrics_refresh_timestamp_seconds` exceeding a
few refresh intervals, or on gRPC/HTTP error ratios.
