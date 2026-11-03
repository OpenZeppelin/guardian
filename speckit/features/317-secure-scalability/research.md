# Research: Security-Preserving Scalability (317)

**Date**: 2026-10-30 | **Spec**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

Code citations are against `main` at `1100f3a4`. Paths are relative to the
repository root; `server/` means `crates/server/src/`.

## Baseline facts the design depends on

| Fact | Evidence |
|---|---|
| Auth does not know the route; it only gets a `session_eligible` bool (`resolve_account` vs `resolve_account_allowing_session`). | `server/services/mod.rs:143,154,167` |
| `/state` and `/state/nonce` (and other reads) sign byte-identical payloads; only `ExecuteDeltaProposal` is domain-separated. | `server/api/http.rs:1088-1119`, `crates/shared/src/auth_request_payload.rs:11-21,55` |
| Session-eligible routes are 13, not the RFC's 10: execute proposal, get proposal execution and get current execution also accept sessions. | `server/services/execute_proposal/service.rs:133`, `server/services/execution_status.rs:9,35` |
| `x-auth-format` parses to `Raw / Eip712 / Session`; unknown values fail as untyped strings (HTTP 401, gRPC `InvalidArgument`). | `server/metadata/auth/credentials.rs:11-25,185,224` |
| Session read order today: metadata SELECT, skew check, P-256 verify (inline), `auth_sessions` SELECT, Guardian/network/cosigner checks, floor UPSERT. | `server/services/mod.rs:173-217,250-335,421` |
| Floor CAS and purge: Postgres `INSERT .. ON CONFLICT DO UPDATE WHERE old < new`; purge deletes `session-%` floors older than `now - (max_ttl + 2*skew)` every 60 s on every replica. | `server/metadata/postgres.rs:359-412`, `server/builder/handle.rs:733-768`, `server/session.rs:294` |
| F2: a handler can pass skew + session checks, stall past the purge cutoff, then CAS against a purged floor and accept a duplicate. Nothing bounds the stall. | `.claude/skills/verify-auth-replay/SKILL.md:199`, `expected.txt` |
| P-256 verify accepts high-S signatures. | `crates/shared/src/session_key.rs:113-129` |
| No metadata version column; `updated_at` also moves on every `has_pending_candidate` flip. | `server/.../schema.rs:102-113`, `server/metadata/postgres.rs:429-494` |
| Signature verification runs inline on the async runtime. | `server/metadata/auth/mod.rs:359`, `server/services/mod.rs:206,262` |
| Canonicalization: CPU gate `cores/2` (deliberately not configurable), background admission gate capacity 1. | `server/network/mod.rs:25-43,90-120` |
| Dashboard stats refresher fetches every account's full state every 300 s, but already reuses the previous decoded vault summary when the commitment is unchanged, so decoding is limited to changed accounts. Metrics refresher calls `metadata.list().len()` on every replica every 30 s. | `server/dashboard/stats.rs:505,880-930`, `server/metrics/refresher.rs:44-82` |
| HTTP/gRPC latency histograms exist (`guardian_http_request_duration_seconds`), but the CloudWatch export uses empty dimensions and sum/count only, and the widget plots the seconds average (0.0 to 0.1 = 0 to 100 ms). | `server/metrics/http.rs:45-70`, `infra/observability.tf:178-187,369-378` |
| Empty gRPC body: tonic returns `Status::internal("Missing request message.")` before any handler, counted as a server fault. | tonic 0.14.6 `src/server/grpc.rs:386`, `server/metrics/grpc.rs:41` |
| Both SDKs mint monotonic timestamps per client instance only; Rust builds a new `GuardianClient` per multisig operation, TS creates several `GuardianHttpClient`s. No per-(account, session) write ordering, no clock correction. | `crates/client/src/client.rs:50-56,160,1021`, `crates/miden-multisig-client/src/client/helpers.rs:52-57`, `packages/guardian-client/src/http.ts:319-335,893-946` |
| TS `fetchSigned` mints the timestamp before `await signRequest`, so concurrent calls can send out of timestamp order. | `packages/guardian-client/src/http.ts:900-905` |
| Benchmark has no reader/writer roles and does not model the nonce poll. | `benchmarks/prod-server/src/workload.rs:3-13`, `config.rs:40-43` |

## R1. Signed envelope

- **Decision**: Implement the RFC's `session-v2` byte layout unchanged:
  `"GUARDIAN_SESSION_REQUEST" || 0x00 || u16be(2) || u8(transport) ||
  u16be(operation) || AuthRequestMessage word (32 bytes)`, P-256 over
  SHA-256 of these bytes. New typed module in `crates/shared` owns encoding;
  `session_key.rs` gains byte-level sign/verify next to the existing
  word-typed API (grant and logout signing unchanged).
- **Rationale**: Reviewed in the RFC; keeps grants and keys valid; fixed
  length, no text conversion.
- **Alternatives**: Signing method + path strings (rejected: proxy prefixes,
  query ordering). Adding a client nonce to the envelope for read uniqueness
  (rejected for now, see R5; keeps the RFC bytes).

## R2. Operation registry and how handlers pass it

- **Decision**: `SessionOperation` enum with explicit `u16` discriminants
  1 to 13 (RFC 1 to 10 plus `ExecuteDeltaProposal = 11`,
  `GetDeltaProposalExecution = 12`, `GetCurrentExecution = 13`), each with a
  `ReplayPolicy::{Read, Write}`. `resolve_account_allowing_session` takes
  the operation instead of a bool; `resolve_account` (wallet-only) takes
  none, so a v2 credential there is refused by type. Transport is set by the
  credential extractor (HTTP extractor vs gRPC metadata).
- **Rationale**: The handler is the only place that knows the route; the
  type makes "session route without an operation" unrepresentable.
- **Alternatives**: Derive from axum `MatchedPath` in middleware (rejected:
  gRPC needs a separate path, and a middleware string map can drift from
  handlers).

## R3. Version cutover and errors

- **Decision**: `RequestAuthFormat::{Raw, Eip712, SessionV2}`. The literal
  `session` is still recognized and mapped to a typed
  `GuardianError::UnsupportedAuthVersion` (HTTP 400, gRPC
  `InvalidArgument`, `retryable = false`) on per-account routes; the logout
  route keeps its own `session` format. Unknown strings keep today's
  behavior.
- **Rationale**: RFC; clients must see a terminal, specific error and must
  not clear the grant or fall back to wallet signing.

## R4. Read duplicate record (Postgres)

- **Decision**: New range-partitioned table `session_read_seen
  (record_key bytea, request_ts_ms bigint, PRIMARY KEY (record_key,
  request_ts_ms)) PARTITION BY RANGE (request_ts_ms)`, partitions of 5
  minutes. `record_key = SHA-256(session_public_key || envelope bytes)`:
  32 bytes that bind the session key and the exact message P-256 signs
  (never signature bytes). The primary key is 40 bytes, instead of about
  112 with a hex `floor_key` text column plus a separate digest, which
  roughly halves index size, WAL volume and buffer footprint at 10,000
  inserts/s. Because the timestamp is inside the signed message, a
  duplicate always lands in the same partition and conflicts on the
  primary key. Two sessions signing identical envelopes get different
  keys.
- **Combined call (phase 4)**: one `plpgsql` function
  `guardian_authorize_and_record_session_read(account_id, session_key,
  record_key, ts, skew_ms)` does the read-auth lookup and the record in
  **one round trip**, in order:
  1. reads `account_metadata.auth_version` and the `auth_sessions` row;
     if the account is missing or the session is missing, expired or
     revoked, returns that outcome **without inserting**;
  2. takes `pg_advisory_xact_lock_shared(K_READ)`;
  3. **admission freshness (FR-011)**: reads `clock_timestamp()` *after*
     the lock is held and returns `stale` if `abs(now_ms - ts) > skew_ms`;
  4. **purge fence (FR-011a)**: returns `stale` if `ts <= watermark`;
  5. `INSERT .. ON CONFLICT DO NOTHING`, returning `(auth_version,
     session row, recorded | duplicate)`.

  Each statement in a volatile function takes a fresh read-committed
  snapshot. Grant Guardian-key, network and cosigner checks stay in Rust
  after the call, against the version-validated cached metadata. If one of
  those fails after the insert, the request is refused and its digest is
  spent; that request was invalid anyway, so this burns nothing a
  legitimate client needs. Phase 3 ships the lookup alone (without the
  record) so it works on v1 auth; phase 4 folds the record in.
- **DB round trips per session read**: two (combined auth and record,
  then the state or state-head read), instead of four today. Folding the
  state-head read in as well is possible for `get_canonical_nonce`, but
  states use a separate pool and module; not planned unless the benchmark
  needs it. The database clock is the single admission
  clock; the server's earlier skew check is only a cheap pre-filter. The
  purge cutoff is computed from the same database clock.
- **Why both checks**: the watermark deliberately trails the window
  (`now - skew - margin`, rounded down to a partition boundary), so the
  fence alone would admit a request that stalled past its window but
  arrived before the next purge. Freshness alone would not cover a clock
  step backwards on the database host. The model checks the two properties
  separately.
- **Purge**: the sweep takes `pg_advisory_xact_lock(K_READ)` (exclusive),
  advances the watermark to a partition boundary at or below
  `now - skew - margin`, commits, then drops partitions entirely below the
  watermark. Future partitions are created ahead (1 h) idempotently by every
  replica's sweep; an insert with no partition fails closed.
- **Why this closes F2 for reads**: an insert either completes under the
  shared lock before the watermark moves (its row is then only removed for
  `ts <= watermark`, and any later duplicate sees the new watermark and is
  `stale`), or starts after and sees the new watermark. There is no window
  where a row is gone but the watermark still admits its timestamp.
- **Rationale**: Exact-duplicate rejection, atomic across replicas,
  bounded storage (`rate x (2 x skew + partition)`, about 7 M rows at
  10 k/s), dropping partitions instead of row deletes avoids vacuum churn at
  10 k inserts/s. Advisory locks live in shared memory, so the shared lock
  does not create row-level MultiXact contention.
- **Alternatives**:
  - Freshness predicate inside the `INSERT` (rejected: evaluated at
    statement start, a statement blocked on a concurrently deleted row can
    still insert after purge).
  - `SELECT .. FOR SHARE` on a watermark row (rejected: MultiXact
    contention on one hot row at 10 k/s).
  - `UNLOGGED` table (rejected: crash recovery truncates it, re-opening
    replay for the window).
  - Plain table with periodic `DELETE` (viable fallback if partition
    management proves fragile; same fence).
  - Window-only reads (rejected by the spec on confidentiality).
  - Per-replica cache (rejected: not fleet-wide).

## R5. Read uniqueness from one client

- **Decision**: Both SDKs keep a strictly increasing millisecond
  timestamp source per **(session, account)**, owned by the session object
  (so shared by every base-client instance using it), used for reads and
  writes of that account. It does not order or serialize reads; it only
  makes two identical reads distinct. A drift guard keeps it within 1 s of
  the local clock: if the next value would be more than 1,000 ms ahead,
  the call waits until the clock catches up.
- **Rationale**: Envelopes only collide when account, transport,
  operation, payload and timestamp are all equal, so reads for different
  accounts never need distinct timestamps. A single per-session counter
  would push timestamps ahead of real time during a multi-account sync
  burst (500 parallel calls move it 500 ms), and sustained > 1,000 calls/s
  would eventually exceed the 300 s future skew. Per-account scoping
  removes that, and the guard bounds the worst case for one very hot
  account.
- **Alternatives**: Random nonce in the envelope (rejected: changes RFC
  bytes; revisit if independent processes sharing one key become a supported
  case).

## R6. Write floor and F2 for writes

- **Decision**: Keep `update_last_auth_timestamp_cas` and the `session-<hash>`
  key unchanged in semantics. Add the same two checks: the CAS runs under
  `pg_advisory_xact_lock_shared(K_FLOOR)`, re-checks freshness against
  `clock_timestamp()` after the lock is held, and refuses `ts <= floor
  watermark`; the session-floor purge takes the exclusive lock and advances
  the watermark to its cutoff before deleting. Filesystem: freshness,
  watermark and CAS under the existing `auth_state` lock, using one clock
  read taken inside the lock.
- **Rationale**: Closes F2 without assuming bounded handler time (RFC
  decision 3); same mechanism as R4, one model.
- **Alternatives**: Request deadline shorter than `ttl + 2*skew` (kept as an
  additional control, not the proof).

## R7. Filesystem backend

- **Decision**: Durable duplicate record. Each accepted read appends
  `(record_key, ts)` to a per-5-minute bucket file under
  `.metadata/session_read_seen/`, flushed before the response, mirrored in
  an in-memory set under the same lock as the watermark. On start, buckets
  above the persisted watermark are loaded; buckets wholly below it are
  deleted by the purge. Freshness and fence checks are the same as
  Postgres.
- **Rationale**: Identical semantics to Postgres (FR-018). The filesystem
  backend is for local development, so a write per read is acceptable.
- **Rejected: start-time watermark.** A read can carry a timestamp up to
  the skew ahead of server time. Sequence: accept a read timestamped 2 min
  ahead, restart 10 s later, re-register the same still-valid grant (the
  in-memory session store forgets it), resend the read. Its timestamp is
  above the start-time watermark and the set is empty, so it would be
  accepted twice.
- **Rejected: session incarnation.** The request does not sign an
  incarnation, so a new incarnation of the same grant cannot distinguish
  the old request.
- **Test**: the sequence above must be rejected on the filesystem backend.

## R8. Metadata cache validated on every read

- **Decision**: Add `auth_version bigint NOT NULL DEFAULT 0` to
  `account_metadata`, incremented by every metadata write except the
  pending-candidate flag and replay CAS (`configure_account` set/create,
  pause, unpause, release set/clear). A new read-authorization lookup
  returns `(auth_version, session row)` in one query (`account_metadata`
  joined with `auth_sessions` for session requests); in phase 4 it becomes
  the combined call from R4. On version match the
  cached `AccountMetadata` is used; otherwise it is reloaded through a
  `moka::future::Cache` with `get_with` (single flight), weighted by size,
  default 256 MB cap. The cache is only reachable through a
  `ReadAuthorizer` type used by read services; write services keep calling
  `MetadataStore` directly.
- **Rationale**: Never stale (validation hits the primary on every
  request); memory holds metadata only; one DB round trip instead of two.
- **Alternatives**: TTL cache (rejected: stale auth), LISTEN/NOTIFY
  (rejected: drops on reconnect), Redis (rejected: extra hop and service).
- **Dependency**: `moka` (new, `future` feature). Alternative `quick_cache`
  if review prefers a smaller crate.

## R9. Signature verification off the runtime

- **Decision**: Run session P-256 and wallet verification through a
  `VerifyPool`: semaphore sized to cores, then `spawn_blocking`. Gate the
  change on the benchmark: keep it only if p99 under burst improves.
- **Rationale**: Matches existing `Reconstructor` and execution patterns
  (`server/network/mod.rs:81-152`).

## R10. Canonicalization concurrency

- **Decision**: Make the background admission gate configurable
  (`GUARDIAN_CANONICALIZATION_REBUILD_ADMISSION`, default 1). Effective
  concurrency is `min(admission, cores/2)`, so a 2-vCPU task still runs
  one rebuild at a time; parallel rebuilds need a leader with at least 4
  vCPUs. The CPU gate stays host-derived. Measure on 4 and 8 vCPU with
  admission 1, 2 and `cores/2`, with 100 concurrent candidates, and record
  foreground `push_delta` p95 at the same time (limit: no more than 20%
  worse). Run `verify-candidate-queue` and `verify-worker-lease`.
- **Sizing outcome**: if one rebuild at a time already meets SC-004 at the
  FR-036 writer load, keep prod at its current size and document that;
  otherwise document the required leader size in
  `docs/SERVER_AWS_DEPLOY.md`.
- **Rationale**: The admission gate is the 100 s worst case; the CPU gate
  protects the runtime and the foreground push path. A prior A/B found
  capacity 1 does not regress bursts, so the default changes only with
  evidence.
- **Alternatives**: Sharding the leader (deferred, spec out of scope).

## R11. 100,000-account jobs

- **Decision**:
  - Release sweep: derive the effective rate as
    `max(configured, ceil(accounts / (rotation / 2)))`, so a pass always
    fits half the rotation.
  - Metrics refresher: replace `metadata.list().len()` with a count query.
  - Dashboard stats: decoding is already limited to accounts whose
    commitment changed. The remaining cost is fetching and decrypting
    every state each refresh. First, read commitments only (as
    `pull_state_head` does) and fetch full state only for changed
    accounts. Second, use SQL aggregates for counts that come from stored
    columns. Vault-derived values keep coming from decoded summaries; they
    cannot be replaced by SQL without storing a summary, which is out of
    scope unless measurement shows the change-only fetch is not enough.
    Keep it leader-leased.
- **Rationale**: Removes O(accounts) memory and decode from periodic jobs.

## R12. Observability

- **Decision**:
  - Add bounded `route` dimension to the CloudWatch export of the existing
    request histograms (matched route template or gRPC method), drop health
    checks (`GET /`, `GetPubkey`), and export percentiles (ADOT `awsemf`
    with histogram values) for p50/p95/p99 widgets per route.
  - New counter `guardian_auth_rejections_total{transport, policy, reason}`
    with `reason in {replay, stale, timestamp_window, invalid_signature,
    unsupported_auth_version, session_invalid}` and `policy in {read,
    write, wallet}`.
  - Counter `guardian_session_read_records_total{outcome}` and a histogram
    for the record call latency.
  - Empty gRPC body: a tower layer on the gRPC service maps tonic's
    `Internal("Missing request message.")` to `InvalidArgument`, with a test
    pinned to the tonic version.
- **Rationale**: Every acceptance number must be readable (SC-008).

## R13. Benchmark

- **Decision**: Extend `benchmarks/prod-server` with separate `reader` and
  `writer` roles, a `nonce_poll` operation (nonce, then state only when the
  nonce changed), a configurable poll interval (default 2 s), writer rate
  and account distribution (default: 100 writers on distinct accounts, one
  push per 10 s; burst leg; hot-account leg of 5 writers on one account),
  100,000-account seeding with parallel shards, a sustained mode (fixed
  duration, at least 30 min), auth rejection counts by reason,
  first-attempt versus retried errors, and server-side DB counters.
- **Latency boundaries**: client-side latency, server request-span latency
  and dashboard latency are reported side by side. Dashboard accuracy is
  judged against server spans (same boundary), not client latency.
  Canonicalization reports accepted-to-canonical and
  chain-inclusion-to-canonical (block inclusion time from the node)
  separately. Compare v1 strict (baseline build at the cut
  commit), v2 strict-only (duplicate record disabled by a test-only flag),
  and v2 with the duplicate record.
- **Rationale**: SC-001, SC-003, SC-006 need these numbers.

## R14. Direct database route

- **Decision**: Set `rds_proxy_route_database_url = false` for devnet and
  prod tfvars (`infra/variables.tf:311`, `infra/data.tf:161-194`) after
  confirming security-group reachability; URL keeps `sslmode=verify-full`.
- **Rationale**: About 13 ms per request, proxy shares nothing (100% pinned).
- **Note**: Operational, applied independently of the release.

## R15. SDK write sequencing

- **Decision**: A `WriteSequencer` owned by the active session object
  (Rust `ActiveSession` in `SessionSlot`; TS `GuardianSession`), keyed by
  account, holding an async lock across timestamp, sign, send and the
  bounded replay retries of a write. Reads bypass it. Same type and
  semantics in both SDKs, covered by matching tests.
- **Rationale**: Per-instance counters are not shared today; the session
  object is the one thing every client instance for that session shares.

## R16. Replay-store capacity gate (before protocol work)

- **Why**: the duplicate record replaces a contended update with an
  insert; it does not remove the write per read. At 10,000 reads/s that is
  about 864 million records per day. Whether it beats today's UPSERT
  depends on index writes, WAL volume, partition rollover and lock cost.
- **Topology**: run on the current prod class `db.r6g.large` (2 vCPU,
  16 GB, single-AZ, 100 GB, per the local prod tfstate snapshot; confirm
  live) and on `db.r6g.xlarge`, from 3 tasks with pool sizes set per R17.
  The result is the smallest class that passes, not a single pass/fail.
- **Failure decision (must be agreed before the gate runs)**: one of
  (1) larger instance up to an agreed monthly cost ceiling, (2) keep strict
  reads (ship v2 and read-path work, drop zero failures for in-client
  parallel reads), (3) reopen window-only reads or response encryption as a
  reviewed security decision. Plus a deadline for the gate.
- **Experiment** (standalone harness, direct route, no protocol change
  needed):
  - 3 writer processes standing in for replicas, 12,000 record calls/s
    combined (20% headroom), realistic key spread (20,000 sessions, 100,000 accounts), 10% of
    calls deliberately duplicated across processes;
  - at least 1 h, so several partition rollovers and purges happen under
    load, plus a forced stall test for freshness and fence;
  - compare against the current UPSERT at the same rate.
- **Pass**: record call p99 <= 10 ms; zero lost or doubled acceptances;
  no errors during rollover and purge; DB CPU and WAL throughput leave at
  least 30% headroom; storage stays flat after the first hour.
- **Fail**: stop and decide among a larger instance, the plain-table
  fallback, or a different design. Window-only reads remain out of scope
  unless separately approved.
- **Formal check, same phase**: model admission freshness, the purge fence
  and the filesystem restart sequence as three separate properties in
  `verify-auth-replay`.

## R17. Connection pools and the rate limiter

- **Pools (fact)**: each replica has two Postgres pools, storage
  (`states`, `deltas`) and metadata (`account_metadata`, sessions), each
  defaulting to 32 in prod (`server/config/stage.rs:61-65`,
  `server/builder/storage.rs:39-115`). Without RDS Proxy, direct
  connections are `2 x 32 x tasks`.
- **Decision**: In phase 1, record `max_connections` of the prod and devnet
  instances and set pool sizes so `tasks_max x (storage + metadata) <=
  80% of max_connections`. Export pool size, in-use and pending-acquire
  metrics per pool, and measure pending acquires during partition
  rollover in the phase 2 experiment. Pool waits count against admission
  freshness, so they also show up as `stale` rejections.
- **Rate limiter (fact)**: every HTTP and gRPC request takes a
  process-wide `std::sync::RwLock::write()` on `RateLimitStore::entries`
  and formats 2 to 4 `String` keys (`server/middleware/rate_limit.rs:270-282`).
  Limits are divided by `GUARDIAN_MAX_REPLICAS`.
- **Decision**: Measure the limiter's lock hold and wait time in the phase 1
  baseline at the target rate. If it shows up in p99, replace the single
  lock with a sharded map (keyed by hash of the limit key) and avoid
  allocating key strings per request. Benchmark profiles and prod config
  must set per-IP and per-key limits that allow 20,000 readers behind a
  few NAT or benchmark-worker IPs, and the report must count rate-limit
  rejections separately so they are not mistaken for auth failures.

## Resolved unknowns

All Technical Context unknowns are resolved above. Measurements still
required: R16 (gate, before protocol work), R9 keep/drop, R10 default and
leader size.
