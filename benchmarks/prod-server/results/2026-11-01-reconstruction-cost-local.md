# State reconstruction cost by account size and phase, 2026-11-01

Where the CPU of one state reconstruction (`MidenNetworkClient::apply_delta`,
run once per `push_delta` and again per canonicalization candidate) goes, on
multisig accounts with a growing executed-transactions replay map. Release
build, Apple M-series laptop, synthetic accounts from
`crates/server/src/testing/apply_delta_bench.rs`. Reproduce with:

```bash
cargo test -p guardian-server --lib --features e2e --release \
    -- --ignored --nocapture apply_delta_cost
```

## End to end, by account size (one-entry delta, 200 iterations)

The three benches ran at once, so these are slightly inflated.

| cosigners / past txs | state size | p50 | p95 |
|---:|---:|---:|---:|
| 3 / 0 | 3 KB | 0.87 ms | 0.95 ms |
| 3 / 1,000 | 89 KB | 14 ms | 16 ms |
| 11 / 10,000 | 858 KB | 112 ms | 129 ms |
| 50 / 10,000 | 865 KB | 111 ms | 124 ms |
| 50 / 50,000 | 4.3 MB | 531 ms | 623 ms |
| 50 / 100,000 | 8.5 MB | 971 ms | 1,229 ms |

A delta near the 1 MB body limit costs 0.86 s on a new account and 1.08 s on a
10,000-transaction one. A full-state (deployment) delta costs about 44 µs.

## By phase (p50, 30 iterations)

| cosigners / past txs | load | apply | commitment | serialize | total |
|---:|---:|---:|---:|---:|---:|
| 3 / 0 | 669 µs | 46 µs | 6 µs | 2 µs | 723 µs |
| 3 / 1,000 | 10.6 ms | 46 µs | 6 µs | 20 µs | 10.7 ms |
| 11 / 10,000 | 101 ms | 49 µs | 6 µs | 188 µs | 101 ms |
| 50 / 50,000 | 495 ms | 57 µs | 6 µs | 986 µs | 496 ms |

## Findings

- Cost grows linearly with the account's executed-transaction count (about
  10 µs per past transaction), not with the delta. Cosigner count barely
  matters.
- Loading the snapshot is 93 to 99.8% of the cost: decoding JSON and base64
  and rebuilding each storage map's Merkle tree from its entries. Applying the
  patch (about 50 µs), the commitment (6 µs) and serializing (at most 1 ms)
  are negligible.
- So any change that avoids loading the snapshot removes nearly all the cost:
  keeping a loaded account in memory, or reusing the post-state a push
  already computed instead of rebuilding it at canonicalization. Applying
  patches more cleverly, or a faster commitment, would not help.
