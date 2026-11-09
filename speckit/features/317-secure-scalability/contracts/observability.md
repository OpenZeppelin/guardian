# Contract: Metrics and Dashboards

Label values are bounded enums. Never use account IDs, public keys,
signatures, digests or payloads as labels.

## New metrics (as implemented)

| Metric | Kind | Labels | Meaning |
|---|---|---|---|
| `guardian_auth_rejections_total` | counter | `credential` (`wallet`, `session`), `reason` (`replay`, `stale`, `timestamp_window`, `invalid_signature`, `session_invalid`, `not_cosigner`) | Every per-account auth rejection |
| `guardian_metadata_cache_requests_total` | counter | `outcome` (`hit`, `reload`, `miss`) | Validated metadata cache |
| `guardian_read_auth_lookup_duration_seconds` | histogram | none | The metadata version lookup on read routes |
| `guardian_db_pool_acquire_duration_seconds` | histogram | `pool` | Wait for a database connection |
| `guardian_rate_limit_lock_wait_duration_seconds` | histogram | none | Wait for the in-process rate-limiter lock |
| `guardian_state_reconstruction_duration_seconds` | histogram | `kind` (`foreground`, `background`) | CPU time of one state reconstruction |
| `guardian_session_read_maintenance_timestamp_seconds` | gauge | none | Last successful session read record maintenance; alarm `<stack>-session-read-maintenance-stale` |

Read record outcomes and fence moves are not separate metrics: duplicates
and fenced reads appear as `guardian_auth_rejections_total{credential="session", reason="replay"|"stale"}`.

## Existing histograms, export change

`guardian_http_request_duration_seconds{method, route}` and
`guardian_grpc_request_duration_seconds{service, method}`: exported to
CloudWatch with the route (or gRPC method) as a dimension, health-check
routes excluded. CloudWatch receives averages only; percentiles are read from
Prometheus (Grafana), see `infra/observability.tf`.

## gRPC status classification

`INTERNAL` with message `Missing request message.` (empty request body) is
rewritten to `INVALID_ARGUMENT` and is not a server fault.

## Dashboards

`infra/observability.tf` and
`docs/guides/observability/grafana/dashboards/guardian.json`: per-route
p50/p95/p99 widgets in Grafana (averages in CloudWatch), auth rejections by
credential and reason, cache hit rate, pool and lock waits, reconstruction
time, and session read maintenance age.

## Session read maintenance

`guardian_session_read_maintenance_timestamp_seconds` (gauge, no labels): Unix time of the replica's last successful maintenance of the session read records. Exported to CloudWatch; alarm `<stack>-session-read-maintenance-stale` fires when its per-period Maximum stops advancing for two 5-minute periods, about 50 minutes before session reads under `unique` would be refused.
