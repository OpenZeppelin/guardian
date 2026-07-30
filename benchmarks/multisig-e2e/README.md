# Guardian multisig end-to-end benchmark

This benchmark measures a real multisig proposal lifecycle. Two persistent 1-of-1 accounts
alternate P2ID transfers through Guardian while the Miden transactions are proved and submitted.
A deterministic fraction of received notes is consumed.

It is intentionally separate from `benchmarks/prod-server`: that harness measures Guardian API
capacity with synthetic deltas, while this one measures the full Rust SDK, Guardian, prover, and
Miden network path.

## Running a scale test end to end

The individual commands are documented below; this is the order they go in, and the two decisions
that determine whether the resulting numbers mean anything.

```bash
# 1. Guardian to measure. Either a local server, or the instrumented two-replica
#    stack if you want server-side metrics and query attribution.
(cd benchmarks/diagnostic-stack && docker compose up -d)

# 2. Provision accounts. Resumable -- re-run it after funding to top up.
cargo run -p guardian-multisig-e2e-benchmark -- prepare \
  --miden-endpoint https://rpc.testnet.miden.io --accounts 8

# 3. Fund every account printed above from the faucet, then give each one a
#    consumable note to start the transfer ring.
cargo run -p guardian-multisig-e2e-benchmark -- bootstrap \
  --config benchmarks/multisig-e2e/testnet.scale.toml

# 4. Optional but usually necessary above ~2 writers: your own prover.
(cd docs/guides/local-prover && docker compose up -d --build)

# 5. Run it.
cargo run --release -p guardian-multisig-e2e-benchmark -- scale-run \
  --config benchmarks/multisig-e2e/testnet.scale.toml
```

**Decision one: `writers` must not exceed proving capacity.** A prover instance proves one
transaction at a time. Two self-hosted replicas at 2 CPU each sustained 4 writers (68 of 68
operations) and failed 8 (23 of 62, proof timeouts). The public prover is far tighter still — 2
writers completed 17 of 17, 16 writers completed 5 of 273. Over-driving it does not degrade
gracefully: completed throughput *fell* from 13.6 to 4.6 operations/min when writers went 4 → 8,
because work that times out consumed a proof slot on the way. Size writers to prover replicas, then
read [Choosing a prover](#choosing-a-prover).

**Decision two: the host must not be saturated.** Total container CPU plus the generator must stay
under physical cores, or a starved generator measures a starved Guardian and the run still looks
healthy from inside the containers. Sample host load *during* the run, not after:

```bash
while pgrep -f scale-run >/dev/null; do
  echo "$(date -u +%H:%M:%S) load=$(sysctl -n vm.loadavg)"; sleep 15
done
```

Load comfortably below core count means the numbers are attributable. On a 10-core machine, a clean
4-writer run sat at 5–7; in-process proving with 8 writers hit 28, and those numbers are not
trustworthy regardless of how good they look.

## Local Guardian workflow

Start a local Guardian server configured for the same Miden network as the benchmark. Create the
two persistent accounts:

```bash
cargo run -p guardian-multisig-e2e-benchmark -- prepare \
  --miden-endpoint https://rpc.testnet.miden.io
```

`prepare` registers the accounts with Guardian and writes their account IDs and Falcon secret keys
to `.guardian/bench/multisig-e2e-accounts.json`. It provisions two accounts by default; pass
`--accounts N` for more, which the scalability work needs to drive more than one concurrent writer:

```bash
cargo run -p guardian-multisig-e2e-benchmark -- prepare \
  --miden-endpoint https://rpc.testnet.miden.io --accounts 16
```

Re-running `prepare` **resumes** rather than restarts: existing entries are never regenerated or
reordered, only missing ones are appended, so an interrupted run cannot strand an account that was
already created, registered, or funded.

Each entry records whether it reached `registered`. An account interrupted between key persistence
and Guardian registration is left `created`, and cannot be registered afterwards -- `push_account`
needs the local account store, which lived in a temporary directory that is gone once the process
exited. `prepare` refuses to continue rather than counting such an entry as done, which would leave
Guardian holding fewer accounts than the fixture claims. Pass `--discard-unregistered` to replace
them; that discards their persisted keys, so it is opt-in. Such an account is unusable and cannot
have been funded, because funding uses the IDs `prepare` prints only on success. Topping up against different endpoints is refused, since
that would mix accounts from two networks into one fixture. The first two accounts keep the labels
`alice` and `bob`; the rest are numbered. The file is mode `0600` on Unix and is ignored by
git. It is not overwritten automatically because the account and secret-key binding must remain
stable.

Each generated account is persisted before it is registered with Guardian. If preparation stops
partway through, the partial fixture retains every key that may have been registered. Preserve or
move that file before deciding whether to reprovision.

Fund both printed account IDs from the faucet selected for the run. Update `testnet.local.toml`
with the faucet's hex or bech32 address, then turn the funding notes into spendable vault assets:

```bash
cargo run -p guardian-multisig-e2e-benchmark -- bootstrap \
  --config benchmarks/multisig-e2e/testnet.local.toml
```

Check balances and connectivity without mutating either account:

```bash
cargo run -p guardian-multisig-e2e-benchmark -- preflight \
  --config benchmarks/multisig-e2e/testnet.local.toml
```

Run the benchmark:

```bash
cargo run --release -p guardian-multisig-e2e-benchmark -- run \
  --config benchmarks/multisig-e2e/testnet.local.toml
```

## Scale run (issue #317 write target)

`run` measures one lifecycle in depth: two accounts alternate transfers sequentially, every stage
timed. It is inherently a pair, so it cannot answer what happens with N writers at once.

`scale-run` answers that. Every fixture account becomes an independent concurrent writer in a ring
-- writer `i` sends to writer `i + 1` -- and each loops propose -> execute -> await canonical for a
fixed window:

```bash
cargo run --release -p guardian-multisig-e2e-benchmark -- scale-run \
  --config benchmarks/multisig-e2e/testnet.scale.toml
```

The loop waits for canonicalization rather than pipelining past it because an account may hold only
one non-canonical delta at a time. Per-writer throughput is therefore an observed property bounded
by canonicalization, not a rate the profile chooses -- which is what "concurrent users transacting"
means for this system.

Each writer runs on its own thread with its own runtime: the multisig client builder holds a
non-`Send` value across an await, so writer futures cannot cross threads, and proving is CPU-bound
so real parallelism is what makes the writers concurrent rather than interleaved.

### Choosing a prover

`prover` decides where proofs are generated, and above a handful of writers it decides what the run
actually measures:

| Value | Proving | Use when |
|---|---|---|
| `"remote"` (default) | the network's shared prover | matching what a real client does, at low concurrency |
| `"local"` | in-process | you need throughput and can accept uncapped CPU |
| `"http://host:port"` | a prover you run | you need proving CPU capped so it cannot starve GUARDIAN |

The shared prover serves one proof at a time per instance, so beyond roughly two concurrent writers
a `remote` run measures the prover rather than GUARDIAN: measured against testnet, 2 writers
completed 17 of 17 while 16 writers completed 5 of 273. `local` removes that ceiling but uses every
core it wants — 8 writers drove host load to 28 on a 10-core machine, which starves the server being
measured. A self-hosted prover is the option that keeps CPU accounting honest; see
[the local-prover guide](../../docs/guides/local-prover/README.md) for the compose stack and sizing.

The summary evaluates three issue #317 criteria directly:

| Criterion | Target | Source |
|---|---|---|
| `canonicalization_p95_ms` | <= 30000 | accepted -> canonical wait, per operation |
| `auth_window_failures` | 0 | failures whose message matches the replay-protection window |
| `concurrent_writers` | configured count | writers that actually produced records |

Each is `pass`, `fail`, or `not_measured`. `not_measured` is never a silent pass: a run where
nothing canonicalized has not shown the latency criterion holds, and a run with no operations has
shown nothing about auth windows.

Auth-window failures are counted separately from every other error because the server returns both
expiry and genuine auth failures as the same error type, differing only in message. Folding them
together would make the "zero auth-window failures" criterion unmeasurable.

Artifacts are `scale-<stamp>-records.jsonl` (one line per operation, including failures) and
`scale-<stamp>-summary.json`.

## Measurements and artifacts

Each completed send is flushed as one JSONL record under `reports/`. A companion manifest captures
the endpoints, public account IDs, workload parameters, and random seed without copying account
secrets. If a run stops early, a failure sidecar captures the operation and full error chain.

The primary timings are:

- `send_proposal_ms`: end-to-end proposal time, including retry attempts and retry sleep.
- `send_proposal_retry_wait_ms`: time spent sleeping between proposal attempts.
- active proposal time: derived as proposal time minus retry sleep.
- `send_execution_ms`: send execution, proving, and submission.
- `note_visibility_ms`: time until the receiver can discover the new note.
- `total_ms`: the complete operation, including optional note consumption.

The summary compares both end-to-end and active proposal medians in the first and last workload
quintiles. The active comparison separates Guardian/client work from configured retry sleep.

Canonicalization observations are queued during the run and collected after foreground account
operations stop. The separate `canonicalization.json` artifact uses Guardian's `canonical_at`
timestamp. A timestamp at or before the local observation start is recorded as zero rather than
falling back to the deferred wall clock. All observations share one `timeout_seconds` drain
deadline, so final collection is bounded independently of observation count.

If Guardian reports that a prior delta is still pending, the benchmark waits
with exponential backoff and jitter, then reruns the proposal workflow against the latest account
state. Proposal-stage Miden RPC `ResourceExhausted`, `Unavailable`, `DeadlineExceeded`, `Internal`,
and `Aborted` responses use the same retry deadline. Execution retries only idempotent Miden stages:
the Guardian delta is pushed once, and ambiguous submissions are reconciled by transaction ID before
the same proven transaction is resubmitted. Sync and consumable-note lookup errors fail the operation
instead of silently falling back to a transfer.

`max_duration_seconds` stops scheduling new operations at an operation boundary. The shared
canonicalization drain runs afterward before the final summary is written.

Summarize a completed or manually interrupted JSONL report:

```bash
cargo run -p guardian-multisig-e2e-benchmark -- summarize \
  --report benchmarks/multisig-e2e/reports/multisig-e2e-<timestamp>.jsonl
```

## Funding calculation

The runner requires a conservative starting vault balance of
`amount * ceil(operations / 2)` for each account. This guarantees that deterministic consume
choices cannot make a run fail for lack of spendable funds. Funding notes do not count until
`bootstrap` has consumed them.

For a quick smoke, lower `operations` before attempting a long run.
