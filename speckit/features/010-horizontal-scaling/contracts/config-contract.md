# Contract: Configuration & Startup Guards

**Feature**: 010-horizontal-scaling

Operator-facing configuration contract. These are the only externally visible
behavior changes (no client wire contract changes).

## Environment variables

| Variable | Status | Behavior |
|---|---|---|
| `GUARDIAN_ENV` | **reused** | Stage signal. `prod` (case-insensitive) activates HA fail-fast guards. Already set from Terraform `var.deployment_stage` (`infra/ecs.tf:128-129`). Currently only gates ACK secrets (`ack/mod.rs:139-145`); `is_prod_environment()` is promoted to a shared `config/stage.rs` helper. |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` | **enforcement changed** | 64-hex (32-byte) shared secret. Prod: **required** — startup fails if unset. Non-prod: optional — warn + ephemeral per-process secret (unchanged dev behavior, `dashboard/state.rs:359-370`). |
| `GUARDIAN_RATE_LIMIT_PARTITIONS` | **new** | Positive integer = the deployment's autoscaling **max** capacity. Divides `GUARDIAN_RATE_BURST_PER_SEC` and `GUARDIAN_RATE_PER_MIN` per replica (`global / partitions`) so aggregate enforcement stays at or below the global limit (stricter when running below max capacity). Defaults from Terraform `effective_server_autoscaling_max_capacity` (see below); operator-overridable. Unset or `1` => current per-process behavior. |
| `DATABASE_URL` | unchanged | Required for the Postgres backend (which the prod image uses). |
| `GUARDIAN_DB_POOL_MAX_SIZE` / `GUARDIAN_METADATA_DB_POOL_MAX_SIZE` | unchanged | Per-replica pool sizes; runbook adds guidance: total ≈ size x replicas x pools must stay under Postgres `max_connections`. |
| `GUARDIAN_RATE_LIMIT_ENABLED` / `GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` | unchanged | Now interpreted as global limits when `GUARDIAN_RATE_LIMIT_PARTITIONS > 1`. |

Optional (implementation may add, with documented defaults): lease TTL / renew
interval overrides (e.g. `GUARDIAN_CANON_LEASE_TTL_SECS`,
`GUARDIAN_CANON_LEASE_RENEW_SECS`). Default to safe values if absent. The lease
TTL is sized for renew/failover only and is independent of the canonicalization
`submission_grace_period_seconds`.

## Terraform wiring (default ships from infra)

`GUARDIAN_RATE_LIMIT_PARTITIONS` MUST default from the deployment's autoscaling
max capacity rather than a manually maintained value:

- `infra/data.tf` already computes
  `local.effective_server_autoscaling_max_capacity` (prod = `max(desired, 6)`).
- `infra/ecs.tf` already injects the rate-limit env block (after
  `GUARDIAN_RATE_PER_MIN`). Add:
  ```hcl
  {
    name  = "GUARDIAN_RATE_LIMIT_PARTITIONS"
    value = tostring(local.effective_guardian_rate_limit_partitions)
  }
  ```
  where `local.effective_guardian_rate_limit_partitions = var.guardian_rate_limit_partitions != null ? var.guardian_rate_limit_partitions : local.effective_server_autoscaling_max_capacity`
  (new `var.guardian_rate_limit_partitions` defaults to `null`, i.e. derive from
  max capacity; operators may override).

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
coordination mode=shared backend=postgres stage=prod rate_limit_partitions=6 cursor_secret=configured
coordination mode=single-process backend=filesystem stage=dev rate_limit_partitions=1 cursor_secret=ephemeral
```

`mode=shared` iff coordination is backed by the external store (Postgres);
`mode=single-process` for the in-memory impls. This is the discoverable signal
that replaces an explicit `DISTRIBUTED_MODE` toggle — coordination capability is
determined by the storage backend, not a separate flag, so the line cannot
disagree with reality.

## Documentation surface (US6)

- `docs/runbooks/horizontal-scaling.md` (new) — required env vars, state-store
  dependency (shared Postgres), pool sizing vs `max_connections`,
  `GUARDIAN_RATE_LIMIT_PARTITIONS` override guidance + rate-limit tolerance
  (under-enforcement below max capacity), filesystem = dev-only, failover behavior
  of the canonicalization lease.
- `docs/CONFIGURATION.md` — add `GUARDIAN_RATE_LIMIT_PARTITIONS` (default from
  autoscaling max capacity), document the prod guards.
- `docs/SERVER_AWS_DEPLOY.md` — HA notes referencing the existing prod profile
  (`infra/data.tf` desired 2 / max 6) and the partition default sourced from it.
