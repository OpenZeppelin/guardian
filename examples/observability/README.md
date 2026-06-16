# `examples/observability`

A local Prometheus + Grafana stack that scrapes a `guardian-server`'s
metrics endpoint and renders a provisioned **Guardian — Server Overview**
dashboard. Use it to see the metric surface from
[#225](https://github.com/OpenZeppelin/guardian/issues/225) without
building dashboards by hand.

The Guardian server itself is **not** part of this stack — run it
separately (host `cargo run`, or the root `docker-compose*.yml`); this
stack only scrapes and visualizes it.

## What's here

```
observability/
├── docker-compose.yml                     # prometheus + grafana
├── prometheus/prometheus.yml              # scrape config (bearer token)
└── grafana/
    ├── provisioning/datasources/…         # points Grafana at Prometheus
    ├── provisioning/dashboards/…          # auto-loads the dashboard
    └── dashboards/guardian.json           # the dashboard
```

## Quick start

1. **Run the server with metrics enabled.** The dashboard expects the
   scrape token `devtoken` (see `prometheus/prometheus.yml`). Bind the
   metrics listener so the Prometheus container can reach it:

   ```bash
   GUARDIAN_METRICS_ENABLED=true \
   GUARDIAN_METRICS_ADDR=0.0.0.0:9464 \
   GUARDIAN_METRICS_BEARER_TOKEN=devtoken \
   GUARDIAN_METRICS_REFRESH_INTERVAL_SECS=15 \
   cargo run -p guardian-server --bin server
   ```

   (Loopback `127.0.0.1:9464` also works on Docker Desktop, but
   `0.0.0.0:9464` is reliable across Docker network setups.)

2. **Start the stack:**

   ```bash
   cd examples/observability
   docker compose up -d
   ```

3. **Open Grafana** at <http://localhost:3001> — anonymous access is
   enabled, so you land directly on the **Guardian / Server Overview**
   dashboard (Dashboards → Guardian). Prometheus is at
   <http://localhost:9090> (check **Status → Targets**: the `guardian`
   target should be `UP`).

4. **Generate traffic** so the panels move — hit the API, exercise the
   dashboard auth flow, etc.:

   ```bash
   curl -s http://127.0.0.1:3000/pubkey >/dev/null
   curl -s "http://127.0.0.1:3000/auth/challenge?commitment=0xdeadbeef" >/dev/null
   ```

5. **Tear down** (add `-v` to also drop the Prometheus/Grafana volumes):

   ```bash
   docker compose down
   ```

## The dashboard

Panels are grouped by subsystem and map 1:1 onto the metric taxonomy in
[`spec/api.md`](../../spec/api.md) (“Metrics Endpoint”):

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

An `Instance` variable (top-left) filters to a single replica or
aggregates across all of them.

## Notes

- **`devtoken` is a throwaway.** It only needs to match the server's
  `GUARDIAN_METRICS_BEARER_TOKEN`. In production, point Prometheus at a
  mounted secret with `authorization.credentials_file:` instead of the
  inline value, and never commit a real token.
- **Anonymous admin** is enabled for zero-friction local viewing. Never
  do this on a Grafana anyone else can reach.
- **Server in Docker instead of host?** If you run `guardian-server` in
  the same Compose network rather than on the host, change the scrape
  target in `prometheus/prometheus.yml` from `host.docker.internal:9464`
  to `<service>:9464` and drop the `extra_hosts` mapping.

See [`docs/OBSERVABILITY.md`](../../docs/OBSERVABILITY.md) for the guide
and [`docs/CONFIGURATION.md`](../../docs/CONFIGURATION.md) (“Runtime —
metrics”) for the full configuration surface.
