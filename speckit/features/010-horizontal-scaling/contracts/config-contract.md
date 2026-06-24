# Contract: Configuration & Startup Guards

**Feature**: 010-horizontal-scaling

Operator-facing configuration contract. These are the only externally visible
behavior changes (no client wire contract changes).

## Environment variables

| Variable | Status | Behavior |
|---|---|---|
| `GUARDIAN_ENV` | **reused** | Stage signal. `prod` (case-insensitive) activates HA fail-fast guards. Already set from Terraform `var.deployment_stage` (`infra/ecs.tf:128-129`). Currently only gates ACK secrets (`ack/mod.rs:139-145`); `is_prod_environment()` is promoted to a shared `config/stage.rs` helper. |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` | **enforcement changed** | 64-hex (32-byte) shared secret. Prod: **required** — startup fails if unset. Non-prod: optional — warn + ephemeral per-process secret (unchanged dev behavior, `dashboard/state.rs:359-370`). |
| `GUARDIAN_MAX_REPLICAS` | **new** | Positive integer = the deployment's autoscaling **max** capacity. Drives **rate limiting only**: divides `GUARDIAN_RATE_BURST_PER_SEC`/`GUARDIAN_RATE_PER_MIN` per replica (`global / GUARDIAN_MAX_REPLICAS`) so aggregate stays at or below the global limit (over-throttles below max capacity). Defaults from Terraform `effective_server_autoscaling_max_capacity` (see below); overridable, but a value **below** the real max makes per-replica caps too high so the aggregate can exceed the global limit (too loose) — Terraform should validate `>=` effective max. A value above the real max over-throttles. Unset or `1` => current per-process rate-limit behavior. **Does NOT affect coordination mode** — that is backend-derived (FR-020). |
| `DATABASE_URL` | unchanged | Required for the Postgres backend (which the prod image uses). |
| `GUARDIAN_DB_POOL_MAX_SIZE` / `GUARDIAN_METADATA_DB_POOL_MAX_SIZE` | unchanged | Per-replica pool sizes; runbook adds guidance: total ≈ size x replicas x pools must stay under Postgres `max_connections`. |
| `GUARDIAN_RATE_LIMIT_ENABLED` / `GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` | unchanged | Now interpreted as global limits when `GUARDIAN_MAX_REPLICAS > 1`. |

Optional (implementation may add, with documented defaults): lease TTL / renew
interval overrides (e.g. `GUARDIAN_CANON_LEASE_TTL_SECS`,
`GUARDIAN_CANON_LEASE_RENEW_SECS`). Default to safe values if absent. The lease
TTL is sized for renew/failover only and is independent of the canonicalization
`submission_grace_period_seconds`.

## Terraform wiring (default ships from infra)

`GUARDIAN_MAX_REPLICAS` MUST default from the deployment's autoscaling max
capacity rather than a manually maintained value:

- `infra/data.tf` already computes
  `local.effective_server_autoscaling_max_capacity` (prod = `max(desired, 6)`).
- `infra/ecs.tf` already injects the server env block (after
  `GUARDIAN_RATE_PER_MIN`). Add:
  ```hcl
  {
    name  = "GUARDIAN_MAX_REPLICAS"
    value = tostring(local.effective_guardian_max_replicas)
  }
  ```
  where `local.effective_guardian_max_replicas = var.guardian_max_replicas != null ? var.guardian_max_replicas : local.effective_server_autoscaling_max_capacity`
  (new `var.guardian_max_replicas` defaults to `null`, i.e. derive from max
  capacity; operators may override).

This keeps the default correct on every deploy with no operator action; the
runbook documents the override, not a required value.

## Startup guards (fail-fast, prod only)

The server MUST refuse to start, with a clear actionable error naming the
variable and remedy, when `GUARDIAN_ENV=prod` and any of:

1. The active storage backend is the **filesystem** backend (US5/FR-012). Remedy:
   build/run with the Postgres backend and set `DATABASE_URL`.
2. `GUARDIAN_DASHBOARD_CURSOR_SECRET` is **unset** (US3/FR-013). Remedy: set a
   stable shared 64-hex secret on all replicas.

In non-prod, condition (1) is allowed (dev default) and (2) warns but starts.

## Error message contract

Each guard error MUST: name the offending variable/backend, state the
consequence under multiple replicas, and give the exact remedy. Errors are
startup/config errors (process exits non-zero), not request-path errors — no
change to HTTP/gRPC boundary error shapes.

## Startup mode log line (FR-019)

On startup the server logs exactly one coordination-mode line reflecting the
**resolved** state (never operator intent):

```text
coordination mode=shared backend=postgres stage=prod max_replicas=6 cursor_secret=configured
coordination mode=single-process backend=filesystem stage=dev max_replicas=1 cursor_secret=ephemeral
```

`mode=shared` iff coordination is backed by the external store (Postgres);
`mode=single-process` for the in-memory impls (filesystem). This is the
discoverable signal that replaces an explicit `DISTRIBUTED_MODE` toggle —
coordination is determined by the resolved storage backend alone, not a flag and
not a tunable, so the line cannot disagree with reality. (`max_replicas` is shown
for the rate-limit context; it does not affect the mode.)

## Documentation surface (US6)

- `docs/runbooks/horizontal-scaling.md` (new) — required env vars, state-store
  dependency (shared Postgres), pool sizing vs `max_connections`,
  `GUARDIAN_MAX_REPLICAS` guidance (rate-limit partitioning only;
  over-throttling/keep-alive tolerance below max capacity; a too-low override can
  let aggregate limits exceed the global limit, a too-high override over-throttles),
  coordination mode is backend-derived (Postgres = shared),
  filesystem = dev-only, failover behavior of the canonicalization lease.
- `docs/CONFIGURATION.md` — add `GUARDIAN_MAX_REPLICAS` (default from autoscaling
  max capacity; rate-limiting effect only — does not change coordination mode),
  document the prod guards.
- `docs/SERVER_AWS_DEPLOY.md` — HA notes referencing the existing prod profile
  (`infra/data.tf` desired 2 / max 6) and `GUARDIAN_MAX_REPLICAS` sourced from it.
