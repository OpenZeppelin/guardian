# Contract: Configuration & Startup Guards

**Feature**: 010-horizontal-scaling

Operator-facing configuration contract. These are the only externally visible
behavior changes (no client wire contract changes).

## Environment variables

| Variable | Status | Behavior |
|---|---|---|
| `GUARDIAN_ENV` | **reused** | Stage signal. `prod` (case-insensitive) activates HA fail-fast guards. Already set from Terraform `var.deployment_stage` (`infra/ecs.tf:128-129`). Currently only gates ACK secrets (`ack/mod.rs:139-145`); `is_prod_environment()` is promoted to a shared `config/stage.rs` helper. |
| `GUARDIAN_DASHBOARD_CURSOR_SECRET` | **enforcement changed** | 64-hex (32-byte) shared secret. Optional in every stage: if unset, warn and fall back to an ephemeral per-process secret (boots, never fails startup). A missing shared secret degrades only dashboard pagination across replicas, so it is not a startup guard (`dashboard/state.rs`). |
| `GUARDIAN_MAX_REPLICAS` | **new** | Positive integer = the deployment's steady-state task capacity: greater of desired count and autoscaling max when enabled, desired count otherwise. Divides both global HTTP limits and dashboard per-commitment auth budgets per replica. The aggregate stays at or below the configured limit in steady state; rolling deployments may temporarily allow up to `deployment_maximum_percent / 100` times that limit. Terraform clamps overrides upward to steady-state capacity. A value above capacity over-throttles. Unset or `1` => current per-process behavior. A set-but-invalid value (non-integer or `0`) fails startup in prod and warns + resolves to `1` in non-prod. **Does NOT affect coordination mode** — that is backend-derived (FR-020). |
| `GUARDIAN_DASHBOARD_COMMITMENT_RATE_BURST_PER_SEC` / `GUARDIAN_DASHBOARD_COMMITMENT_RATE_PER_MIN` | **new** | Positive integer fleet-wide budgets for each dashboard auth endpoint and commitment. Defaults to `6` burst/second and `30`/minute. Each is divided by `GUARDIAN_MAX_REPLICAS` and clamped to at least 1 per replica for login liveness. Custom budgets below the divisor may exceed their nominal fleet-wide value because of the liveness clamp. |
| `DATABASE_URL` | unchanged | Required for the Postgres backend (which the prod image uses). |
| `GUARDIAN_DB_POOL_MAX_SIZE` / `GUARDIAN_METADATA_DB_POOL_MAX_SIZE` | unchanged | Per-replica pool sizes; runbook adds guidance: total ≈ size x replicas x pools must stay under Postgres `max_connections`. |
| `GUARDIAN_RATE_LIMIT_ENABLED` / `GUARDIAN_RATE_BURST_PER_SEC` / `GUARDIAN_RATE_PER_MIN` | unchanged | Now interpreted as global limits when `GUARDIAN_MAX_REPLICAS > 1`. |

Optional (implementation may add, with documented defaults): lease TTL / renew
interval overrides (e.g. `GUARDIAN_CANON_LEASE_TTL_SECS`,
`GUARDIAN_CANON_LEASE_RENEW_SECS`). Default to safe values if absent. The lease
TTL is sized for renew/failover only and is independent of the canonicalization
`submission_grace_period_seconds`.

## Terraform wiring (default ships from infra)

`GUARDIAN_MAX_REPLICAS` MUST default from steady-state capacity rather than a
manually maintained value:

- `infra/data.tf` computes
  `local.effective_server_steady_capacity` from the greater of desired count and
  autoscaling max when autoscaling is enabled, or the desired count otherwise.
- `infra/ecs.tf` already injects the server env block (after
  `GUARDIAN_RATE_PER_MIN`). Add:
  ```hcl
  {
    name  = "GUARDIAN_MAX_REPLICAS"
    value = tostring(local.effective_guardian_max_replicas)
  }
  ```
  where `local.effective_guardian_max_replicas = var.guardian_max_replicas != null ? max(var.guardian_max_replicas, local.effective_server_steady_capacity) : local.effective_server_steady_capacity`
  (new `var.guardian_max_replicas` defaults to `null`; an explicit override is
  clamped **up** to steady-state capacity so it can only raise the divisor).

This keeps the default correct on every deploy with no operator action; the
runbook documents the override, not a required value.

Terraform also injects the two configurable dashboard commitment budgets. With
the prod defaults, `GUARDIAN_MAX_REPLICAS` resolves to 6, so one replica permits
1 request per second and 5 requests per minute for each auth endpoint and
commitment. At the default 200% deployment maximum, a rolling deployment may
temporarily allow up to 2× the configured aggregate rate.

The managed prod profile also resolves a pre-created Secrets Manager entry
(default `${stack_name}/server/dashboard-cursor-secret`) and injects its value as
`GUARDIAN_DASHBOARD_CURSOR_SECRET` into every ECS task. Planning or deploying
prod fails when the entry is absent. The deployment helper provides the
one-time, non-overwriting `bootstrap-dashboard-cursor-secret` command. This is a
Terraform profile guarantee, not a new server startup guard: manually managed
deployments may still omit the variable and receive the ephemeral-key warning
described above.

## Startup guards (fail-fast, prod only)

The server MUST refuse to start, with a clear actionable error naming the
variable and remedy, when `GUARDIAN_ENV=prod` and any of:

1. The active storage backend is the **filesystem** backend (US5/FR-012). Remedy:
   build/run with the Postgres backend and set `DATABASE_URL`.
2. An enabled global rate limit partitions to **zero** requests per replica
   (`GUARDIAN_RATE_BURST_PER_SEC`/`GUARDIAN_RATE_PER_MIN` below
   `GUARDIAN_MAX_REPLICAS`, FR-013). Remedy: raise the global limit or lower
   `GUARDIAN_MAX_REPLICAS`.

In non-prod, condition (1) is allowed (dev default) and (2) warns but starts. A
missing `GUARDIAN_DASHBOARD_CURSOR_SECRET` is NOT a startup guard in any stage:
it warns and boots with an ephemeral per-process secret (FR-008), because it
degrades pagination only.

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
