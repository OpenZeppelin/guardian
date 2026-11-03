# Feature 317 sustained comparison (local), 2026-11-02

Same load against four server configurations, to compare the 317 changes
with main and the two session read policies. Local A/B, not an absolute
capacity measurement. The `strict` read policy was removed after this run,
so its leg describes a build that no longer exists.

## Setup

- Apple M-series laptop running the server, Postgres 16 (Docker) and the
  load generator together. Release builds from two worktrees: main at
  `1100f3a4` (baseline) and the same commit plus the uncommitted 317 changes.
  Both builds had canonicalization switched off through a local-only patch
  (synthetic accounts never land on chain); everything else is the shipped
  code.
- Profile: 2,020 ECDSA users. 2,000 readers poll the canonical nonce every
  2 s and fetch state when it changed; 20 writers write every 10 s; 60 of the
  readers are cosigners of writer accounts, so they fetch state after each
  write. A write is a proposal, its approval and the push. 60 s warmup,
  540 s measured. Fresh database per leg, rate limits lifted, pool size 32.
- Harness: `benchmarks/prod-server` `worker-run`, one shard.

## Results

All legs ran at the profile's rate with zero failures and zero
authentication rejections: 1,000 nonce polls/s, 6 state fetches/s, 2 writes/s.

| Leg | Server CPU | Nonce poll p50 / p95 / p99 | State fetch p50 / p95 / p99 | Write p50 / p95 / p99 |
|---|---:|---|---|---|
| main, wallet | 36% | 1.92 / 4.32 / 22.2 ms | 1.67 / 3.43 / 9.99 ms | 20.6 / 29.2 / 38.7 ms |
| 317, wallet | 37% | 2.16 / 7.07 / 11.1 ms | 2.04 / 4.70 / 7.56 ms | 24.0 / 36.4 / 47.8 ms |
| 317, session, `strict` | 53% | 3.37 / 19.2 / 56.6 ms | 2.72 / 13.0 / 35.9 ms | 26.1 / 48.3 / 225 ms |
| 317, session, `unique` | 42% | 1.53 / 2.90 / 5.09 ms | 1.46 / 2.12 / 2.98 ms | 20.6 / 25.2 / 30.4 ms |

Server side, per nonce poll (317 legs, from the server's metrics):

| Leg | Mean server time | Metadata-pool checkouts | Mean pool wait |
|---|---:|---:|---:|
| wallet | 2.63 ms | 2.05 | 0.33 ms |
| session, `strict` | 7.54 ms | 3.07 | 1.38 ms |
| session, `unique` | 1.51 ms | 1.05 | 0.23 ms |

The metadata cache served 603,537 reads from cache against 1,960 misses
(about one per account) in the `strict` leg.

## Findings

- Session reads under `unique` have the lowest latency and the fewest
  database calls of any configuration: one call (the combined session
  admission) instead of three under `strict` (metadata version, session
  lookup, fenced floor update) or two for a wallet read (metadata version,
  timestamp floor). Mean server time drops from 7.5 ms (`strict`) to 1.5 ms,
  below the wallet path's 2.6 ms, and p99 from 57 ms to 5 ms. They are not
  the cheapest in CPU: the server used 42% of a core in that leg against 37%
  for wallet requests on the same build (not profiled; likely P-256
  verification and the record insert), so this run shows a latency and database-load gain, not lower CPU.
- Sessions under `strict` are the most expensive configuration, mostly from
  queueing: three checkouts per request and a pool wait four times the
  others'. With a 32-connection pool at 1,000 requests/s this is what pushes
  p95 and p99 up. `strict` also writes on every read (the floor update), so
  `unique`'s insert per read does not add database writes; it replaces one.
- Wallet traffic is unchanged by 317 within this single run's noise: same
  CPU, p50 0.2 to 3.4 ms higher (writes 20.6 to 24.0 ms), nonce p95 4.3 to
  7.1 ms, p99 for reads lower. One run per leg cannot separate these from
  noise.
- No replay rejections in any leg: each reader sends one request at a time,
  so `strict` never saw reordered timestamps. Clients that read in parallel
  would add `authentication_replay` retries under `strict` on top of the cost
  above.

## Caveats

- One run per leg on one machine shared with the load generator; differences
  of a few milliseconds at p50 are within noise. The `strict` against
  `unique` gap (5x mean server time, 3x checkouts) is well outside it.
- Canonicalization was off, so the background worker's database and CPU use
  is not included, and accepted-to-canonical time is not measured.
- 2,020 users, not the 20,100 of the target profiles; absolute capacity needs
  a deployed run, planned separately.
- Artifacts (profiles, worker artifacts, server metrics, CPU samples) were
  kept in the session scratchpad, not in the repository.
