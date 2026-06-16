# Observability

Guardian exposes operational telemetry in Prometheus text exposition
format. This page is the operator-facing guide: how to turn metrics on,
how to scrape them, and how to visualize them. For the wire-level
contract (endpoint semantics, the full metric taxonomy, cardinality
rules) see [`spec/api.md`](../spec/api.md) “Metrics Endpoint”; for the
env vars see [Configuration](./CONFIGURATION.md#runtime--metrics-prometheus).

## What you get

When `GUARDIAN_METRICS_ENABLED=true`, the server runs a **dedicated
metrics listener** (default `127.0.0.1:9464`, separate from the API
port) serving `GET /metrics`. It instruments, end to end:

- **HTTP and gRPC request paths** — rate, latency histograms, in-flight
  depth, status/code breakdown (labels are route *templates*, never raw
  paths).
- **Miden RPC** — outbound calls to the chain node, the upstream
  dependency the server's availability hangs on.
- **Storage** — per-operation latency and outcomes, plus per-pool
  (`storage`/`metadata`) connection-pool saturation on Postgres builds.
- **Canonicalization** — run rate/duration, candidate outcomes, retries.
- **Delta & proposal lifecycle**, **account growth**, **operator auth**,
  **rate limiting**, and the **refresher health** (staleness + failures).
- **Process metrics** (`process_*`: CPU, RSS, file descriptors).

Two design points worth knowing as an operator:

- **Scrapes are cheap.** Expensive cross-account aggregates (delta
  counts, in-flight proposals, account totals, pool status) are computed
  by a background refresher every
  `GUARDIAN_METRICS_REFRESH_INTERVAL_SECS` and published as gauges — a
  scrape never touches the database. Staleness is observable as
  `time() - guardian_metrics_refresh_timestamp_seconds`; refresh
  failures increment `guardian_metrics_refresh_failures_total`.
- **Cardinality is bounded by construction.** Every label value comes
  from a closed set (route templates, a gRPC method allowlist, small
  enums). No account IDs, nonces, keys, IPs, or error strings ever
  become labels.

## Enabling and scraping

Run the server with the listener bound where your scraper can reach it,
and (recommended) a shared-secret token:

```bash
GUARDIAN_METRICS_ENABLED=true \
GUARDIAN_METRICS_ADDR=0.0.0.0:9464 \
GUARDIAN_METRICS_BEARER_TOKEN=<token> \
cargo run -p guardian-server --bin server
```

Point Prometheus at it:

```yaml
scrape_configs:
  - job_name: guardian
    authorization:
      credentials: <GUARDIAN_METRICS_BEARER_TOKEN>   # or credentials_file:
    static_configs:
      - targets: ["guardian-host:9464"]
```

The endpoint is operator-only and additive — it is **not** part of the
client SDK contract and does not change any `/dashboard/*` behavior.

### Protecting the endpoint

Defense is layered (see [`spec/api.md`](../spec/api.md) and the
[security note in Configuration](./CONFIGURATION.md#runtime--metrics-prometheus)):

1. **Network isolation first** — the listener binds loopback by default;
   in production keep `9464` reachable only from the scraper's network
   (private subnet / security group / sidecar).
2. **Bearer token second** — `GUARDIAN_METRICS_BEARER_TOKEN` gates
   scrapes with a constant-time check (`401` otherwise).
3. **TLS** — terminate at a reverse proxy or sidecar where transport
   encryption is required.

Never expose `/metrics` to a public network.

## Visualizing — example Grafana stack

[`examples/observability`](../examples/observability/README.md) is a
ready-to-run Prometheus + Grafana stack with a provisioned **Guardian —
Server Overview** dashboard whose panels map 1:1 onto the metric
taxonomy. Bring up a local server with metrics enabled, then:

```bash
cd examples/observability
docker compose up -d
# Grafana → http://localhost:3001   (anonymous, lands on the dashboard)
# Prometheus → http://localhost:9090
```

The dashboard is a starting point — copy `grafana/dashboards/guardian.json`
into your own Grafana and adapt it. An `Instance` variable filters to one
replica or aggregates across all of them (the metric set is designed for
multi-replica aggregation).

## Out of scope (for now)

Alert rules, recording rules, and runbooks are not shipped here — they
are a planned follow-up. The
[metric taxonomy in `spec/api.md`](../spec/api.md) is the reference for
writing your own (e.g. alert on
`guardian_db_pool_pending_acquires` sustained above zero, on
`time() - guardian_metrics_refresh_timestamp_seconds` exceeding a few
refresh intervals, or on gRPC/HTTP error ratios).
