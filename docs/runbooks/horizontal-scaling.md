# Runbook: Horizontal Scaling (multiple Guardian replicas)

Guardian runs as 2–6 ECS tasks behind a round-robin load balancer in the prod
profile. This runbook covers what an operator must configure for a correct
high-availability (HA) deployment and how the server behaves across replicas.
Tracking: issue #242.

## TL;DR — required for a correct multi-replica deployment

| Setting | Why it matters across replicas |
|---|---|
| **Postgres backend** (`DATABASE_URL`) | Sessions, login challenges, and the canonicalization lease live in Postgres so they are shared. The filesystem backend is **dev-only** and is refused at startup in the prod stage. |
| **`GUARDIAN_DASHBOARD_CURSOR_SECRET`** (64 hex chars) | Pagination cursors are signed with this key. If it differs per replica, a cursor minted on one replica fails on another. **Required** in the prod stage — startup fails if unset. |
| **`GUARDIAN_ENV=prod`** | Activates the prod-stage startup guards (filesystem refusal, cursor-secret requirement). Set by Terraform from `var.deployment_stage`. |
| **`GUARDIAN_MAX_REPLICAS`** | Rate-limit partitioning divisor (see below). Defaults from the autoscaling max capacity via Terraform. |

With the published Postgres image + the prod Terraform profile, all of these are
set for you. The rest of this doc is for understanding and for non-default
deployments.

## Coordination is backend-derived (not a tunable)

The coordination mode is determined by the **storage backend alone**:

- **Postgres backend → shared coordination** (replica-safe). Always. No tunable
  can turn this off — a missing or wrong env var can never silently revert a
  Postgres deployment to per-process state.
- **Filesystem backend → in-memory, single-process** coordination (dev only).

The startup log emits one line reflecting the resolved state, e.g.:

```text
coordination mode=shared backend=postgres stage=prod max_replicas=6 cursor_secret=configured
```

If you ever see `mode=single-process backend=filesystem` on a deployment you
believe is multi-replica, that deployment is **not** safe to run with more than
one task.

If the server is auto-built with the Postgres backend but coordination handles
were not wired (only possible via a manual/embedded builder), it **fails to
start** rather than falling back to per-process state.

## Behavior across replicas

- **Operator & EVM login**: a challenge issued on one replica verifies on any
  other; an established session is honored everywhere; logout and expiry are
  effective fleet-wide.
- **Canonicalization** runs on exactly one replica at a time via a Postgres
  lease (`worker_leases`). Leadership transfers automatically to another replica
  within one lease TTL (≈ 3× the canonicalization check interval) if the holder
  crashes. A superseded holder cannot commit a canonical write (a fencing token
  is checked before every state/delta write).
- **Rate limiting** is per-process but partitioned (see below).

## Failure modes (by design)

- **Shared store (Postgres) briefly unavailable → auth fails closed.** Login and
  authenticated requests are rejected (never bypassed) until Postgres returns.
  The canonicalization leader steps down rather than risk double-processing, and
  resumes automatically. This is a deliberate change from the old always-
  available in-memory behavior.
- **DB connection budget**: each replica opens up to `GUARDIAN_DB_POOL_MAX_SIZE`
  (default 32 in prod) connections, plus the metadata pool, plus per-request
  session lookups. With N replicas the total can approach
  `N × (pools × size)`; keep it under Postgres `max_connections`. Prod routes
  through RDS Proxy by default, which pools server-side and absorbs much of this.

## `GUARDIAN_MAX_REPLICAS` and rate limiting

The configured global limits (`GUARDIAN_RATE_BURST_PER_SEC`,
`GUARDIAN_RATE_PER_MIN`) are divided by `GUARDIAN_MAX_REPLICAS` so each replica
enforces `global / GUARDIAN_MAX_REPLICAS`. With round-robin distribution the
fleet aggregate stays at or below the global limit.

- Default = the deployment's **autoscaling max capacity** (Terraform). It must be
  the *max*, not the count you happen to run now — partitioning by max is
  conservative.
- **Drives rate-limiting only.** It has no effect on coordination mode.
- **Tolerance band**: when fewer than max replicas are running, the fleet
  over-throttles (stricter than the global limit) — accepted. HTTP keep-alive can
  also pin a client to one replica, throttling it at `global / max` (e.g. 1/6) —
  also accepted; it is fail-closed (never too loose).
- **Override** (`var.guardian_max_replicas`): an explicit value is clamped **up**
  to the autoscaling max, so it can never drop below real capacity (which would
  let the aggregate exceed the global limit). Setting it higher only
  over-throttles.

## Filesystem backend is dev-only

The filesystem backend keeps state local to one task (and does not persist audit
events). In the prod stage the server **refuses to start** on the filesystem
backend with an actionable error. Use it only for local development / single
process.
