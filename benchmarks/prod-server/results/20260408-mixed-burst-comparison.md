## Mixed Burst Comparison

Workload:
- `1 push_delta : 4 get_state`
- burst mode
- `4096` total accounts
- `16` ECS worker tasks

### ECDSA Only

- Run ID: `20260408T151157Z-7fdbac04`
- Report: [run-report.json](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T151157Z-7fdbac04/run-report.json)
- Summary: [summary.md](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T151157Z-7fdbac04/summary.md)
- Result:
  - `get_state`: `1137.14 ops/s`
  - `push_delta`: `284.28 ops/s`
- Caveat: recovered from `15/16` worker artifacts after one shard hit a transient `502 Bad Gateway` during configure

### Falcon Only

- Run ID: `20260408T155822Z-6b1f5622`
- Report: [run-report.json](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T155822Z-6b1f5622/run-report.json)
- Summary: [summary.md](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T155822Z-6b1f5622/summary.md)
- Result:
  - `get_state`: `1507.51 ops/s`
  - `push_delta`: `376.88 ops/s`
- Caveat: recovered from `15/16` worker artifacts after one shard hit a transient `502 Bad Gateway` during configure

### Mixed Falcon + ECDSA

- Run ID: `20260408T161344Z-a78b2def`
- Report: [run-report.json](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T161344Z-a78b2def/run-report.json)
- Summary: [summary.md](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T161344Z-a78b2def/summary.md)
- Result:
  - `get_state`: `1409.67 ops/s`
  - `push_delta`: `352.42 ops/s`
  - `get_state` per scheme: `704.84 ops/s`
  - `push_delta` per scheme: `176.21 ops/s`
- Note: full `16/16` worker aggregation completed cleanly

### Cleanup

- [20260408T151157Z-7fdbac04/cleanup-manifest.json](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T151157Z-7fdbac04/cleanup-manifest.json): complete
- [20260408T155822Z-6b1f5622/cleanup-manifest.json](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T155822Z-6b1f5622/cleanup-manifest.json): complete
- [20260408T161344Z-a78b2def/cleanup-manifest.json](https://github.com/OpenZeppelin/private-state-manager/blob/benchmarks-against-prod/benchmarks/prod-server/reports/20260408T161344Z-a78b2def/cleanup-manifest.json): complete
