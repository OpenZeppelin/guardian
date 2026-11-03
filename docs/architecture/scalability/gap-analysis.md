# Guardian scalability target: gap analysis

Date: 2026-10-29. Based on code on `main` (ba910918), the April 2026 prod
benchmark, and devnet/prod metrics and logs from a live qualification run
against devnet on the same day. Design notes, not a committed plan.

The prioritized recommendations are at the end, in
[Recommendations](#recommendations).

The [session-auth RFC](./session-auth-rfc.md) develops the proposed auth
change and records its compatibility, confidentiality, and validation
requirements. It is a draft, not an approved implementation plan.

## Target

| Dimension | Target |
|---|---|
| Guarded accounts | 100,000 |
| Concurrent users transacting (push/canonicalize deltas) | up to 100 |
| Concurrent users reading state (`get_state`) | up to 20,000 |

Acceptance at target load:

- `get_state` p95 <= 1 s at 20k concurrent readers.
- Delta canonicalization p95 <= 30 s at 100 concurrent writers.
- Zero auth-window (replay-protection) failures.

### Open question: the target needs a request rate

"20,000 concurrent readers" is 20,000 req/s at one poll per second and 2,000
req/s at one poll per 10 s. Sizing differs by 10x, so the target should state
a poll interval. This document assumes one poll every 2 to 5 s, i.e. **4,000
to 10,000 `get_state`/s**.

## Where we are today

April 2026 prod benchmark (`benchmarks/prod-server/results/20260408-*.md`):
1 task (2 vCPU / 4 GB), RDS Proxy, 16 client workers, 4,096 accounts.

| Run | push_delta/s | get_state/s | get_state p50 / p95 | push_delta p50 / p95 |
|---|---:|---:|---|---|
| Pure-write ECDSA burst | 612.65 | 0 | - | - / 4,156 ms |
| Mixed 1:4, Falcon+ECDSA | 352.42 | 1,409.67 | 718 / 926 ms | 2,972 / 3,935 ms |
| Low-pressure reference | - | - | 37 / 60 ms | 212 / 228 ms |

In the mixed burst the single task was saturated at about 1,400
`get_state`/s, with p95 already near 1 s. The target is 3 to 7 times that.

Measured on Oct 29 (devnet and prod span logs):

- RDS Proxy adds about 13 ms to every request (read p50 9 ms direct vs about
  22 ms through the proxy, on both devnet and prod). Both proxies have 100%
  of connections pinned, so they provide no connection sharing.
- Storage encryption costs nothing measurable. KMS ECDSA ack signing adds
  about 20 to 25 ms to `push_delta` only.
- CPU time per request is 0.5 to 6 ms. The rest of each request is waiting on
  the database, Miden RPC and KMS.

## Gap 1: zero replay-protection failures (hardest criterion)

### How it works today

- Each (account, signer) pair has a timestamp floor in `account_auth_state`
  (`crates/server/src/metadata/postgres.rs:359-393`).
- A request is accepted only if its timestamp is **strictly greater** than
  the floor (`ON CONFLICT ... WHERE last_auth_timestamp < EXCLUDED...`).
  The request must also be within ±5 min of server time
  (`MAX_TIMESTAMP_SKEW_MS`, `metadata/auth/credentials.rs:6`).
- Session-signed requests use their own floor key (`session-<hash>`), with
  the same strictly-increasing rule.

### Why it fails at scale

- Two requests from the same signer with the same millisecond timestamp are
  rejected with `AuthenticationReplay`. So are requests that arrive out of
  order, which is normal with multiple replicas or a client sending in
  parallel.
- With 20,000 readers, any client that reads in parallel, or retries, will
  hit this. Tuning alone can't get to zero.

### What's needed

- **Option A, nonce store:** replace the timestamp floor with a set of
  request hashes seen inside the ±5 min window, and reject only exact repeats.
  Parallel and out-of-order requests then pass. This is a wire contract
  change (`AGENTS.md` §4). The store must key on the signed message digest,
  never on signature bytes: the server accepts both the low-S and high-S
  form of a P-256 signature (`accepts_high_s_signatures_from_webcrypto`,
  `crates/shared/src/session_key.rs:113`), so anyone can produce a second
  valid signature for the same request without the key.
- **Option B, client discipline:** the SDKs send each signer's requests one
  at a time with timestamps that always increase. No protocol change, but it
  caps per-signer read rate, and Rust and TypeScript must behave identically
  (`AGENTS.md` §3 rule 5).
- **Option C, split by route class (recommended, see below):** reads check
  the timestamp window only, writes keep the strict floor.
- **Either way:** a metric for `authentication_replay`, and a counter in the
  benchmark harness. Today "zero failures" isn't measurable directly.

### Does a session read need replay protection?

What a signature covers: account ID, timestamp and request payload
(`AuthRequestMessage`, `crates/shared/src/auth_request_message.rs`).

What replay protection guards against: someone holding a copy of a signed
request, but not the key, sending it again. Realistic sources are request
headers logged at a TLS-terminating hop (Cloudflare, ALB), browser
extensions, devtools or HAR files, and client-side logging. It does not
help against an attacker who has the session key itself; they can sign new
requests. Session expiry, scope and revocation already bound that case.

- **Session reads.** A replay changes nothing; its only effect is that the
  replayer receives the account state. That matters for private accounts,
  where state confidentiality is part of what Guardian provides. A leaked
  header log usually has no response bodies, so a replay is how an attacker
  would get the state. Without the floor, a copied request is usable only
  within its timestamp window and only for the account it names. The strict
  floor costs a DB write per read and rejects legitimate parallel or
  out-of-order reads, which is the zero-failures criterion.
- **Session writes.** Most writes are already idempotent: a replayed
  `push_delta` hits the nonce/commitment check, a replayed signature hits
  "already signed", and re-sending `abandon_candidate` for the same nonce
  does nothing new. The strict floor is cheap protection for anything that
  isn't idempotent, and writes are bounded by the 100-writer target, so it
  doesn't matter for scaling.

**The signature does not cover the route.** These all sign the same payload,
`{account_id}`:

- `get_state` and `get_canonical_nonce` (`StateQuery`, `api/http.rs:54`)
- `get_delta_proposals` (`ProposalQuery`, `api/http.rs:96`)
- `get_delta_history` with no cursor or limit (`HistoryQuery`)

The gRPC versions have the same property, since identical protobuf messages
encode to identical bytes. Today the strict floor hides this: a timestamp is
usable once on any route. All routes with that shared payload are reads, and
write routes sign distinct payloads (nonce, delta, signer commitment), so
nothing is exploitable now. If reads stop advancing the floor, protection
would rest on payloads never colliding. **Binding method and path into the
signed message must be part of the same change** (contract change, both
SDKs).

### Recommended shape

1. **Reads:** timestamp-window check only, no DB write, with a shorter
   window for session reads (the RFC proposes 60 s instead of ±5 min). A
   short window needs SDK clock correction from server time, because
   neither SDK corrects for clock offset today. Don't make authorization
   depend on a per-replica cache of seen requests: it can't reject fleet-wide
   duplicates.
2. **Writes:** keep the strict floor.
3. **Signed message:** bind the operation (the RFC uses a registry of
   operation IDs, not raw paths).
4. **Wallet-signed reads:** could take the same split, or keep the strict
   floor if the SDKs move readers to sessions.

Whether a short-lived leaked read is acceptable for private accounts is a
security decision for the RFC.

### Removing the need for read replay protection

- **Public accounts are not exempt.** Their state is on chain, but
  `get_delta_proposals`, `get_delta_proposal` and the delta routes return
  pending proposals and deltas that are not on chain, even for public
  accounts. A replayed read on those routes reveals off-chain data, so an
  account-type exemption is wrong.
- **Encrypt read responses** (ECDH + AES-GCM or HPKE). A replayer without
  the private key gets ciphertext, so replay is harmless with no window and
  no stored state. About 0.1 ms CPU per response. **It cannot reuse the
  session signing key:** the browser session key is a non-extractable ECDSA
  key, and WebCrypto refuses ECDH derivation with it (algorithm and key
  usages are both checked). It needs a separate encryption key that the
  grant authenticates, plus response binding, rotation and downgrade
  protection. That likely changes the grant and the wallet prompt, so it is
  a separate proposal and probably a breaking change, not a small additive
  one.
- **Authenticated streams instead of polling:** one challenge-signed gRPC
  stream (SSE/WebSocket for TypeScript) per reader, with state pushed on
  change. The challenge is single-use and connection-bound, held in memory.
  Also the largest scaling win (connections, not polls per second), and the
  largest change.
- **Channel binding** (signing something unique to the TLS session) does
  not work: TLS terminates at Cloudflare and the ALB.

### Formal check

[`tla/`](./tla/README.md) holds a TLA+ design model that compares these
designs. Summary: today's design fails the zero-failures criterion with no
attacker; relaxing reads to a window check fixes it; binding the route is
required for any relaxation (and closes a latent colliding-write hazard);
encrypting responses is the only variant where every property passes. The
model's encryption switch is idealized: it says nothing about how the key
is established, and the current browser session key cannot do it (see
above).

### 0.19 scope

The part that has to land in a major release is binding the operation into
the signed message, for session-signed requests only (session keys are
SDK-local; wallet-signed messages go through Miden Wallet, Ledger and other
signers, and changing them needs wallet compatibility work). The
[RFC](./session-auth-rfc.md) specifies it as a `session-v2` envelope with an
operation registry and a version. It can ship with strict replay checks
first; enabling window-only reads is a separate reviewed policy change with
the same wire bytes, gated on closing F2 in the code model. The cutover
rejects the old session format on per-account routes, so browser apps on
old SDKs lose session reads until they upgrade. Response encryption needs a
grant change and is a separate proposal. Streams are a new API. Before
landing: update the `verify-auth-replay` model, the cross-SDK test vectors,
`docs/MIDEN_COMPATIBILITY.md`, and get a security review.

## Gap 2: `get_state` p95 <= 1 s at 20k readers

### Cost of one `get_state` today (`services/get_state.rs:23-46`)

1. SELECT `account_metadata`.
2. Timestamp skew check (CPU).
3. Signature verification, inline on the async runtime thread
   (`metadata/auth/mod.rs:359`, no `spawn_blocking`). The session path adds
   a SELECT on `auth_sessions`.
4. Replay UPSERT on `account_auth_state` (**a database write per read**).
5. SELECT `states`, then decryption.

That's 3 reads and 1 write against Postgres per request, with nothing cached
in memory. At 5,000 req/s that's 5,000 writes/s for replay floors alone.

### What's needed, roughly in order of effect

1. **Remove RDS Proxy** (`rds_proxy_route_database_url = false`, keeps
   `sslmode`). About 13 ms per request.
2. **Stop writing to the database on reads.** Window-only session reads
   (gap 1, the RFC's proposal), or a shared nonce store as the
   security-preserving alternative. This is a security decision and should
   be made explicitly.
3. **Cache account metadata in memory, checked on every read** (see
   "Caching design" below).
4. **Skipping unchanged state already exists.** Both multisig SDKs poll
   `get_canonical_nonce` before fetching state (TypeScript `multisig.ts:828`,
   Rust `client/account.rs:513`), and that route returns the stored nonce
   and commitment without loading the state blob. So most polls are already
   cheap on the state side; what remains per poll is the auth work (metadata,
   session lookup, replay write). The benchmark must model this nonce-poll
   flow, not only full `get_state` calls.
5. **Scale out.** With 2 and 3 done, a read is mostly signature verification
   (about 0.1 to 0.3 ms CPU). Several 1 to 2 vCPU tasks should cover 10,000
   req/s. Move signature verification to `spawn_blocking` or a bounded CPU
   pool so bursts don't stall the runtime.
6. **Read replica for state reads**, only if the database is still the limit
   after the above.

### Caching design

**Capacity is not the problem.** Metadata is mostly signer commitments,
threshold or policy, scheme and flags: a few KB per account, so 100,000
accounts is roughly 100 to 300 MB per replica. Use a size-bounded LRU so a
deploy can't OOM a task. A hit costs microseconds. Each replica caches
separately, which multiplies memory, not database load.

**Risks:**

1. **Stale auth decisions (the serious one).** Replica A commits a
   `configure_account` that removes a signer; replica B keeps authorizing the
   removed signer until its copy is invalidated. The qualification
   remove-signer scenarios assert immediate refusal, so a TTL-only cache
   breaks them. (Pause is not affected: paused accounts stay readable by
   design, per `docs/DASHBOARD.md` "Account pausing", and mutations already
   re-read the pause status at write time. Don't add read denial through a
   cached pause flag.)
2. **Caching state is risky and unnecessary.** States grow with history
   (about 84 B per transaction, issue #320 soak), so 100,000 states could be
   gigabytes. A cached state can be out of date, and it keeps decrypted state
   in long-lived memory, which weakens encryption at rest.
3. **Cold starts and deploys.** 20,000 readers against empty caches hit the
   DB at once. Use single-flight loading (e.g. moka `get_with`) so concurrent
   misses for one account share a fetch.
4. **Write-path correctness.** The push gate, CAS updates and canonicalization
   must keep reading from the DB. A cached value reaching those paths would
   break the guarantees the TLA+ models (`verify-*` skills) check.
5. **Cross-replica invalidation.** LISTEN/NOTIFY drops notifications when a
   connection drops, so it needs flush-on-reconnect plus a TTL backstop.

**Recommended: a cache validated on every read.**

- Each read does **one compact lookup** that returns the account's metadata
  version and, for session requests, the live session row (expiry and
  revocation). A metadata version alone cannot prove the session is still
  live, so the session check stays in the lookup; it can be one joined query.
  - Version matches the cached one: authorize from the cached metadata.
  - Otherwise refresh the metadata first.
  - Then serve the route as today (`get_canonical_nonce` without loading
    the state blob; `get_state` loads it).
- Results:
  - **Never stale:** the check runs on every request against the primary,
    so a signer removal or session revocation takes effect immediately on
    every replica, with no LISTEN/NOTIFY. A lagging read replica cannot
    serve this lookup.
  - **DB work per read** goes from 3 reads + 1 write today (session
    `get_state`) to one compact lookup, plus the state read on `get_state`.
  - **Memory** holds metadata only, never state.
  - The lookup must keep the current error distinctions (expired, revoked,
    not a cosigner) and fail closed if it errors.
- Cost: one DB round-trip per read remains, about 1 ms without RDS Proxy.
  Postgres should sustain 10,000 such lookups/s on a reasonable instance; a
  benchmark should confirm.
- Needs a metadata version column (or equivalent) bumped by every metadata
  write.

**Redis instead?** One place to invalidate, but it adds a network hop (about
1 ms, like the proxy cost we measured), another service to run, and still has
the staleness question. With validate-on-read, Postgres already plays that
role, so Redis isn't needed.

## Gap 3: canonicalization p95 <= 30 s at 100 writers

The push side is fine (about 600 `push_delta`/s in April). The limits are in
the worker:

- **One leader.** Canonicalization runs on one replica under a lease on
  `worker_leases` (`coordination/postgres/lease.rs`).
- **Rebuilds are serialized.** `apply_delta` has an admission semaphore of 1
  plus a CPU semaphore of cores/2 (`network/mod.rs:70-130`). Rebuilds take
  1 ms to 1 s, so 100 candidates at once is up to about 100 s worst case.
  That alone could miss 30 s.
- **Chain floor.** A candidate can't be canonical before the Miden block that
  includes it. Outside Guardian's control.
- Per candidate: `pull_state` (DB), one `apply_delta` (CPU), one Miden RPC
  (`get_account_commitment`), plus DB writes on promotion. Full pass every
  10 s, fast promotion every 3 s for candidates up to 30 s old.

### What's needed

- Raise or adapt the rebuild admission limit (measure first).
- If one leader isn't enough, split the work across replicas by account (a
  lease per account group).
- Keep `GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS` at 50 or higher.
  An earlier c50 run reached about 33.5 effective concurrency, so this part
  may already be close.

## Gap 4: 100,000 accounts

Most paths only touch accounts with candidates and scale fine. Jobs that scan
all accounts:

- **Release sweep:** one pass per 6 h rotation at 5 accounts/s. 100,000 / 5 =
  20,000 s, about 5.6 h, which barely fits. Raise the rate or the rotation
  first.
- **Dashboard stats refresher and metrics gauges:** run aggregate queries
  over all accounts. Check query plans and indexes at 100k rows.
- Wallet replay floors are never purged (only session floors are), so
  `account_auth_state` grows with accounts × signers. Fine at 100k, but worth
  noting.

## Gap 5: proving it

- **Percentile metrics per route.** The dashboard latency widgets show HTTP
  averages of 0.0 to 0.1 ms while span logs show 20 to 100 ms for the same
  requests, so acceptance numbers can't be read from them today.
- **Extend `benchmarks/prod-server`:**
  - seed 100,000 accounts (April used 4,096)
  - distributed clients simulating 20,000 readers at the agreed poll rate
  - 100 concurrent writers
  - report `get_state` p95, accepted-to-canonical p95 (the harness already
    samples this) and replay error count
- **Run on a prod-like stack:** several tasks, direct DB, Multi-AZ if prod
  will be. Add one change at a time so each effect is measurable.

## Note on instance count

A second instance adds little burst capacity on its own:

- The server mostly waits on I/O, not CPU.
- The rate limit is a fleet total, divided across replicas
  (`middleware/rate_limit.rs:159`).
- Background work runs on one leader.
- Writes to one account are serialized.

Its value is availability: rolling deploys, surviving a task crash, and
running in two zones. After gaps 1 and 2 are closed, reads become CPU bound
and stateless, and that's when more instances add real read capacity.

## Is fixing replay protection and canonicalization enough?

No. They are necessary but not sufficient.

- **Replay redesign** unlocks the replay criterion and removes the DB write
  per read.
- **Canonicalization concurrency** unlocks the 30 s p95.
- **Reads are a throughput problem neither change solves.** At 4,000 to
  10,000 `get_state`/s, each read would still do 2 lookups, a decryption and
  a signature check: 8,000 to 20,000 SELECTs/s against a server that
  saturated at about 1,400 reads/s in April. Closing that needs the proxy
  removal, the validated metadata cache, scale-out, and possibly a read
  replica for state reads.
- **Also required:** the 100k-account sweep fixes, and percentile metrics plus
  a target-shaped benchmark to prove anything.
- **Outside Guardian:** Miden block inclusion bounds canonicalization latency,
  and Miden RPC capacity under load.

With only the first two, expect the replay and canonicalization criteria to
pass and `get_state` to miss p95 <= 1 s at the target rate.

## Recommendations

In order. Each item says what it fixes and the evidence behind it.

### Now: operations, no release needed

1. **Route the database connection directly, not through RDS Proxy, on
   both stacks.**
   - Change: `rds_proxy_route_database_url = false`. The URL keeps
     `sslmode` (`infra/data.tf:172`), so the connection stays encrypted.
   - Why: both proxies have every connection pinned
     (`DatabaseConnectionsCurrentlySessionPinned` equals
     `ClientConnections` every hour), so they share none, and they add
     about 13 ms to every request (reads 9 ms direct, about 22 ms through
     the proxy on both devnet and prod).
   - Before applying: check that the prod security group lets the tasks
     reach the database directly.
2. **Per-route latency percentiles on the dashboard.** The current latency
   widgets show 0.0 to 0.1 ms while requests take 20 to 100 ms, so no
   acceptance criterion can be checked from them.
3. **Small fixes:**
   - Return `invalid_argument`, not `internal`, for a gRPC request with an
     empty body; today it counts toward the server-fault alarm.
   - Build `guardian-client` before the TypeScript qualification leg, so a
     stale `dist` cannot cause false failures.
   - Report an operator login refused by the allowlist as
     `environment_blocked` in `live-account-paused-1of1-ecdsa`, not as a
     product failure.
4. **Run prod as 2 smaller instances, not 1 larger one.** For availability
   (deploys without downtime, surviving a task crash, two zones), not burst
   capacity: the rate limit is a fleet total and background work runs on
   one leader. Keep storage encryption (no measurable cost) and KMS (about
   20 to 25 ms per push only).

### In 0.19: breaking changes only

5. **Bind the operation into the signed message, for session-signed
   requests only** (the [RFC](./session-auth-rfc.md)'s `session-v2`
   envelope).
   - Why: every relaxation of read replay protection depends on it, and it
     closes the latent read/write payload collision. Both are shown by the
     [design model](./tla/README.md).
   - Why sessions only: session keys are SDK-local, while wallet-signed
     messages go through Miden Wallet, Ledger and other signers, which is a
     separate compatibility project.
   - The envelope carries a version, and session creation advertises the
     server's read policy.
   - Ship it with strict replay checks; it can go out before the window
     decision is made.
   - Cost: browser apps on old SDKs lose session requests at the server
     cutover until they upgrade. Name that in the release notes.
   - Before release: update the `verify-auth-replay` model, the cross-SDK
     test vectors and `docs/MIDEN_COMPATIBILITY.md`, and get a security
     review.
   - If 0.19 can't wait for it, session read relaxation slips to 0.20.

### After 0.19: policy and server changes

6. **Window-only session reads**, as a reviewed policy change on the v2
   wire format:
   - Session reads check only a short timestamp window; writes keep the
     strict floor. This meets the zero-failures criterion and removes the
     DB write per read. No account-type exemption: public accounts return
     off-chain proposals and deltas too.
   - Gates: close F2 in the code model, SDK clock correction from server
     time (neither SDK has it), and a security decision that a copied read
     may stay usable for the window.
   - Prefer an asymmetric window (about 60 s into the past, a few seconds
     into the future) to halve how long a copy stays usable.
7. **Response encryption**, as its own proposal: a separate encryption key
   authenticated by the grant (the session signing key can't do ECDH in
   WebCrypto). The only design where every property in the model holds,
   but likely a grant change and a breaking release.
8. **Read path:** the metadata cache validated on every read (signer
   removal and revocation take effect immediately, state never cached).
   With item 6, a session read goes from 3 DB reads and 1 write to one
   compact lookup plus the state read where needed.
9. **Canonicalization:** allow more than one rebuild at a time (today one,
   up to about 100 s worst case at 100 writers). If needed later, shard the
   leader by account group. Check both with the existing
   `verify-candidate-queue` and `verify-worker-lease` models.
10. **100,000 accounts:** speed up the release sweep (5.6 h of its 6 h
    rotation at that size) and check the plans of full-account queries.

### Later

11. **Authenticated streams instead of polling** for readers. The biggest
    scaling change: 20,000 readers become open connections, not thousands
    of polls per second. Do it if benchmarks show polling dominates after
    items 6 to 8.
12. **Prove the target** with a benchmark shaped like it: 100,000 accounts,
    20,000 readers at the agreed rate, 100 writers, reporting `get_state`
    p95, accepted-to-canonical p95 and replay error count.

### Summary

| # | Work | Fixes | When | Kind |
|---|---|---|---|---|
| 1 | Direct DB route, drop RDS Proxy | about 13 ms per request | Now | Terraform |
| 2 | Per-route latency percentiles | Measuring anything | Now | Observability |
| 3 | gRPC status, harness fixes | Alarm noise, false failures | Now | Server, harness |
| 4 | 2 smaller prod instances | Availability | Now | Infra |
| 5 | Operation binding, `session-v2` envelope (strict checks) | Prerequisite for 6, colliding-write hazard | 0.19 | Breaking contract change, security review |
| 6 | Window-only session reads | Replay criterion, DB write per read | After 0.19, gated on F2, clock correction, security call | Server policy + SDK clock correction |
| 7 | Response encryption with a grant-authenticated key | Read leak on replay | Separate proposal | Grant + contract change, both SDKs |
| 8 | Validated metadata cache | Read throughput | After 0.19 | Server |
| 9 | Canonicalization concurrency | Canonicalization p95 | After 0.19 | Server, TLA+ |
| 10 | Sweeps at 100k accounts | Account count | After 0.19 | Server |
| 11 | Authenticated streams | Polling load | Later | New API |
| 12 | Target-shaped benchmark | Proof | Alongside 6 to 10 | Tooling |

If only three things get done: drop the proxy now, ship operation binding
in 0.19, and build the read path plus the benchmark after it.

### Decisions needed

1. **Reader poll rate.** Turns "20,000 readers" into a request rate
   (2,000 to 20,000 req/s, a 10x sizing difference).
2. **0.19 cut date versus operation binding** (item 5), including whether
   breaking old browser apps' sessions at the cutover is acceptable.
3. **Session reads:** a short window (item 6), or keep strict reads until
   a response-encryption proposal (item 7) exists. Whether a short-lived
   leaked read is acceptable for private accounts is a security call.
4. **Multi-AZ for the prod database.** The only remaining reason to keep a
   proxy.
5. **Redis:** not needed with the validated cache; revisit only if Postgres
   cannot sustain the per-read lookup.

Items 5 to 8 are design decisions with security impact, so they fit an RFC.
