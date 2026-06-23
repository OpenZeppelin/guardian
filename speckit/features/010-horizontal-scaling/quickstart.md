# Quickstart: Validating Horizontal Scaling Locally

**Feature**: 010-horizontal-scaling

Goal: stand up two Guardian server instances against one shared Postgres and a
single front door, then verify the P1/P2 acceptance scenarios. This is the
manual analogue of the multi-replica integration tests.

## Prerequisites

- Postgres reachable (one shared instance).
- Server built with `feature = "postgres"`.
- A stable cursor secret: `export GUARDIAN_DASHBOARD_CURSOR_SECRET=$(openssl rand -hex 32)`.

## 1. Start two replicas against one DB

```bash
export DATABASE_URL=postgres://...           # same DB for both
export GUARDIAN_DASHBOARD_CURSOR_SECRET=...   # same value for both
export GUARDIAN_ENV=prod                       # exercise the prod guards
export GUARDIAN_RATE_LIMIT_PARTITIONS=6   # = autoscaling MAX capacity (NOT how many you run now)

# replica A
GUARDIAN_HTTP_PORT=8080 cargo run -p guardian-server --features postgres &
# replica B (same env, different port)
GUARDIAN_HTTP_PORT=8081 cargo run -p guardian-server --features postgres &
```

Set `GUARDIAN_RATE_LIMIT_PARTITIONS` to the deployment's autoscaling **max**
capacity (e.g. 6), independent of how many replicas you happen to run locally —
the value must not track the current/running replica count.

Migrations run automatically at startup, serialized by a Postgres advisory lock,
so launching both replicas at once is safe (one migrates, the other waits then
no-ops); both processes then see `auth_sessions`, `auth_challenges`,
`worker_leases`. To exercise the race deliberately, start A and B simultaneously
against a fresh DB and confirm both come up with the schema applied once.

Each replica logs one coordination-mode line at startup (FR-019) — confirm it
reads `coordination mode=shared backend=postgres ...` here (it would read
`mode=single-process backend=filesystem ...` for a dev/in-memory run).

Put a round-robin proxy in front (or just hit A and B directly to force the
cross-replica cases).

## 2. US1 — login across replicas

- Request an operator challenge from **A** (`:8080`).
- Submit the signed response to **B** (`:8081`).
- Expect: verification succeeds, session established (FR-001).
- Make an authenticated call to **A** with the session from **B** — accepted
  (FR-002).
- Log out on **A**; reuse the token on **B** — rejected (FR-003).

Check the DB: `SELECT realm, count(*) FROM auth_sessions GROUP BY realm;`

## 3. US2 — canonicalize exactly once + failover

- Create pending candidates (use the existing demo/smoke flow).
- Observe `worker_leases`: exactly one `holder_id` for `canonicalization`.
- Confirm each candidate is promoted/discarded once (no duplicate submissions in
  logs across A and B) (SC-002).
- Kill the lease holder; within the lease TTL the other replica acquires the
  lease (`holder_id` changes, `fence_token` increments) and canonicalization
  resumes with no manual action (SC-003).
- Briefly pause Postgres mid-run: the holder fails to renew, steps down, and no
  duplicate submission occurs (the fence check blocks any in-flight write by a
  superseded holder); work resumes when the DB returns (FR-005/FR-018).

## 3b. Shared-store outage (auth fails closed)

- Briefly make Postgres unavailable and issue an authenticated request: it is
  **rejected** (auth fails closed), not allowed through; requests succeed again
  once the DB returns (FR-018).

## 4. US3 — pagination cursors across replicas

- With the shared cursor secret set, page 1 from **A**, page 2 (using A's cursor)
  from **B** — correct continuation (SC-004).
- Restart with `GUARDIAN_DASHBOARD_CURSOR_SECRET` unset and `GUARDIAN_ENV=prod` —
  startup **fails** with an actionable error (FR-013).
- Same unset secret with `GUARDIAN_ENV` non-prod — starts with a warning.

## 5. US4 — rate limiting across replicas

- With `GUARDIAN_RATE_LIMIT_PARTITIONS` set to the max capacity and a known global
  limit, drive traffic through the round-robin front door above the limit; confirm
  the aggregate accepted rate stays at or below the global limit (not ~2x), and is
  stricter than the global limit when running fewer replicas than the partition
  count (SC-005).

## 6. US5 — filesystem backend refused in prod

- Build/run the **filesystem** backend with `GUARDIAN_ENV=prod` — startup fails
  fast naming the backend and the remedy (SC-006).
- Same with `GUARDIAN_ENV` non-prod — starts normally (dev path preserved).

## 7. No-regression (single replica)

- Run one replica with the filesystem backend and no new env vars — behavior is
  identical to today; existing single-replica test suites pass unchanged (SC-008).

## What "done" looks like

All of SC-001..SC-008 demonstrable; OpenAPI drift gate shows no diff; Rust/TS
operator + multisig smoke flows pass unchanged (no client wire-contract drift).
