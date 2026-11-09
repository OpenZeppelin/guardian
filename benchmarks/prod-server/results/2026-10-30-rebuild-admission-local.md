# Canonicalization rebuild admission: local scheduling measurement, 2026-10-30

How the background admission gate (`GUARDIAN_CANONICALIZATION_REBUILD_ADMISSION`)
trades background drain time against foreground `push_delta` waiting, for 4 and
8 vCPU leaders. Reproduce with:

```bash
cargo test -p guardian-server --lib rebuild_admission_scheduling -- --ignored --nocapture
```

## Setup

The real process-wide `Reconstructor` with its CPU gate sized for each host
(half the cores) and the background admission under test. 100 background
rebuilds of 40 ms are submitted at once, as the canonicalization worker does;
meanwhile a foreground reconstruction of 10 ms (a `push_delta`) is started every
20 ms. Work is a timed sleep, so this measures the gate's scheduling only, not
CPU contention or real rebuild cost.

## Results

| vCPU | CPU slots | admission | drain s | push wait p50 ms | p95 ms | max ms |
|---:|---:|---:|---:|---:|---:|---:|
| 2 | 1 | 1 | 5.5 | 22 | 24 | 43 |
| 4 | 2 | 1 | 4.3 | 0 | 0 | 0 |
| 4 | 2 | 2 | 2.5 | 15 | 22 | 43 |
| 8 | 4 | 1 | 4.3 | 0 | 0 | 0 |
| 8 | 4 | 2 | 2.2 | 0 | 0 | 0 |
| 8 | 4 | 3 | 1.5 | 0 | 0 | 0 |
| 8 | 4 | 4 | 1.2 | 13 | 17 | 41 |

## Findings

- Drain time scales with admission: about `rebuilds x rebuild time / admission`.
- While at least one slot stays free, a push never waits for background
  rebuilds. This run starts one push at a time; with many concurrent pushes on
  a large leader, `auto` leaves them fewer slots than before (one on 8 vCPU),
  so they can queue behind each other. Not measured here. Taking
  every slot makes pushes wait up to one rebuild (p95 about half a rebuild).
- On a 2 vCPU leader there is one slot, shared by both paths: pushes wait for
  rebuilds whatever the admission.

## Decision

The default becomes `auto`: every slot but one (`cores / 2 - 1`, at least 1).
That is 1 on 2 and 4 vCPU leaders (unchanged) and 3 on 8 vCPU. A number still
overrides it.

## Leader size for the 30 s canonicalization target

With `auto`, draining `N` concurrent candidates takes about
`N x rebuild time / (cores / 2 - 1)`, before Miden RPC and block inclusion.
For 100 candidates:

| Rebuild time | 2 or 4 vCPU (1 slot) | 8 vCPU (3) | 16 vCPU (7) |
|---:|---:|---:|---:|
| 50 ms | 5 s | 1.7 s | 0.7 s |
| 300 ms | 30 s | 10 s | 4.3 s |
| 1 s | 100 s | 33 s | 14 s |

Real rebuild times depend on account state size and come from the end-to-end
benchmark; until then, size the leader from this table.
