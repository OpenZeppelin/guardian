---
name: verify-worker-lease
description: Model-check the Postgres worker lease (issue #242) and the fenced writes it guards with TLA+/TLC before merging a change to lease acquire, renew, release, fence_token handling, the lease TTL or renewal cadence, holder_id generation, lease_fence_is_current, or the lock-then-fence shape of any fenced storage write. Use when a diff touches crates/server/src/coordination/postgres/lease.rs, coordination/leader.rs, the renewal or pass loop in jobs/canonicalization/worker.rs, lease_fence_is_current in storage/postgres.rs, the stats store's lease_is_current/publish, or when check.sh reports DRIFT.
---

# Verify the worker lease

`model/WorkerLease.tla` models the `worker_leases` row, three replicas
and the database clock. TLC explores every interleaving of replicas
acquiring the lease, renewing it (succeeding, losing it, or hitting store
errors), stalling while the clock runs, and running fenced writes, each of
which is lock, fence check, then write and commit. A restarted process is
a new replica (a fresh `holder_id`). One model step is one SQL statement
or one transaction step.

## What is checked

| Property | In plain words | Harm if broken |
|---|---|---|
| `SingleBelievedHolder` | At most one replica runs a pass on a lease it believes is unexpired. | Two replicas canonicalize concurrently, each sure it is the only one. |
| `WritesInTokenOrder` | Fenced writes commit in fence-token order: once a newer holder has committed a write, no write from a superseded holder commits after it. | A stale leader overwrites decisions the new leader already made (lost updates; the corrupted-canonical-row trace in `verify-candidate-queue`). |
| `OneHolderPerToken` | All writes under one fence token come from one replica. | The token stops identifying a leader. |
| `TokenAdvancesOnSteal` | The token never goes back, and every change of holder advances it. | A superseded holder's token becomes valid again. |

`verify-candidate-queue` relies on `WritesInTokenOrder`. It models a stale
promotion that may commit only before the new holder's first locked step.
If this property changes, rerun both skills.

## Does my change touch this?

Yes if the diff changes any function in `mirrored.txt` (the drift check
lists them), or adds a new fenced write. A new fenced write must keep the
modeled shape: take a row lock that every competing writer also takes,
check the fence with `lease_fence_is_current` (or the stats store's
`lease_is_current`) in the same transaction after that lock, then write.
Checking the fence before the lock, or in a separate statement or
transaction, breaks `WritesInTokenOrder` (mutation `m2` below).

## Run it

From the repo root:

```bash
.agents/skills/verify-worker-lease/check.sh          # quick tier, seconds
.agents/skills/verify-worker-lease/check.sh --deep   # longer clock, about 1 min
```

A FAIL verdict names the property TLC reports violated
(`FAIL:WritesInTokenOrder`; any temporal property is `FAIL:temporal`),
and an expected failure in `expected.txt` must name it too, so a
different violation in a cell that is expected to fail still shows as a
`DIFF`. `--cell` exits 0 on PASS, 1 on FAIL and 2 on ERROR.
`--accept-drift` refuses to record a mirrored function it cannot find
(renamed or removed): fix `mirrored.txt` first. Set `TLC_WORKERS` to cap
TLC's worker threads (default: all cores).

It needs `java` and `python3`. `tla2tools.jar` 1.7.4 is cached in
`~/.cache/guardian-tla/` and checksum-verified (downloaded on first run).
Exit codes are the same as `verify-candidate-queue`: 0 all match, 1 a
verdict changed, 2 setup error, 3 mirrored Rust drifted. Full trace for
one bound: `check.sh --cell 9 4`.

## Reading a result

A `DIFF` to FAIL prints the counterexample as model actions (map below).
If every step is something the code can do, it is a real bug: report it
in code terms and keep `expected.txt` and the properties as they are. If a
step is impossible in the code, the model is wrong: fix the action, cite
the Rust, rerun. After changing the model to match intended code, run
`check.sh --accept-drift`.

## Action map

Paths are relative to `crates/server/src/`.

| Model action | Code |
|---|---|
| `Tick` | the database clock (`now()`, `clock_timestamp()`); a replica taking no steps while it advances is a stalled or partitioned replica |
| `Acquire`, `StealAllowed` | `coordination/postgres/lease.rs` `try_acquire` (one INSERT ... ON CONFLICT DO UPDATE ... WHERE expires_at < now() OR same holder; token +1 only on a change of holder), called by `jobs/canonicalization/worker.rs` `run_worker` per pass |
| `RenewHeld`, `RenewLost`, `RenewError` | `lease.rs` `renew` driven by `worker.rs` `spawn_renewal` (definitive loss cancels; second consecutive store error cancels) |
| `Lock` | `storage/postgres.rs` `lock_account_metadata` (or the stats control row `FOR UPDATE`) |
| `FenceCheck`, `FenceOk` | `storage/postgres.rs` `lease_fence_is_current`; `coordination/postgres/stats_store.rs` `lease_is_current` |
| `Commit` | the write and COMMIT of `promote_candidate`, `discard_candidate`, `update_candidate_status`, stats `publish` |
| `EndPass` | the pass returns; `run_worker` cancels and awaits the renewal task; `release` is never called in production |

## Scope and assumptions

- Three replicas, TTL 3 ticks, clock up to 9 (quick) or 12 (deep) ticks,
  up to 4 or 5 committed writes. One lock-protected resource stands for
  any one account (writes to different accounts never contend).
- `holder_id` is unique per process (`builder/storage.rs`: pid plus a
  random u64), so replicas never share one.
- Renewal timing is unconstrained: a renew may happen at any moment or
  never, which covers every TTL/interval ratio, including the code's 3x.
- Passes within one replica are sequential (`run_worker` awaits each).
- Unfenced writes (the pending-flag clears, the release sweep) are out of
  scope here; they rely on their own conditional writes.
- `stats_store.rs` `mark_refresh_started` checks the lease inside a single
  UPDATE's `EXISTS`; under READ COMMITTED that subquery keeps the
  statement snapshot if the UPDATE waits on the control row, so it can
  accept a lease stolen during the wait. It only marks who started a
  refresh, and `publish` re-checks the fence under the lock, so it is not
  modeled.

## Guards the model shows are load-bearing

`promote_candidate` gained a `PromotableKind::Superseded` arm
(`restore_superseded`, restoring an archived superseded delta). It runs
inside the same transaction after `lock_account_metadata` and
`lease_fence_is_current`, so it keeps the modeled lock-then-fence shape;
the fingerprints were re-recorded after that review.

Each of these breaks a property (mutations run against this model):

- `m1` fence checks expiry only (no holder, no token): `WritesInTokenOrder`.
- `m2` fence checked before taking the lock: `WritesInTokenOrder`.
- `m3` steal ignores expiry: `SingleBelievedHolder`.
- `m4` steal keeps the token: `TokenAdvancesOnSteal`.

The three fence conditions (holder, token, expiry) are mutually
redundant: dropping any one alone keeps every property, and so does
keeping only the token. Removing two of them, as `m1` does, breaks it.
