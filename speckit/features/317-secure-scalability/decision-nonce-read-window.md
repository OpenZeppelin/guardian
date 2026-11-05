# Decision: window-only session nonce polls

Status: **Proposed**, for security review. Implemented in #587 so review can
read the code; the "Security posture of this feature" section of
[spec.md](./spec.md) names it as a pending exception. If review rejects it,
the change is reverted before merge.

Date: 2026-11-04. Evidence:
[deployed benchmark](../../../benchmarks/prod-server/results/2026-11-04-devnet-317-bench.md).

## Context

Feature 317 admits every signed session read at most once, fleet-wide, by
recording it in `session_read_seen` (`guardian_admit_session_read`). On the
deployed bench (`db.m7g.large`, normal commits) that record is the limit:

- Each read is a durable insert and commit. The committed build saturates at
  about 4,100 reads/s on the WAL flush; with the typed-query fix, about 4,850.
- Database CPU per read is about 0.30 ms with the fix. 10,000 reads/s on two
  vCPUs allows 0.20 ms at 100% utilization, 0.14 ms with 30% headroom.
- Under read overload, writes miss their schedule (r4: 4.97 of 7.5 writes/s),
  so read cost limits write capacity too.

The traffic is almost all nonce polls. In every profile, readers poll
`get_canonical_nonce` and fetch state only after it changes: r4 served 7,102
nonce polls/s against 15 state fetches/s. Removing the write from the nonce
path alone removes it from nearly all reads.

Locally, the same admission without the insert costs 81 us of database CPU
per read against 132 us with it (39% less, Postgres 17 on two CPUs; ratio,
not an AWS capacity prediction).

## Proposed decision

Session-v2 `GetCanonicalNonce` (gRPC and `GET /state/nonce`) becomes
**window-only**: a valid request is accepted every time it is presented
within its timestamp window, instead of once. Nothing else changes.

| Request | Replay rule | Changes? |
|---|---|---|
| Session-v2 canonical nonce | accepted while its timestamp is in window | **yes** |
| Session-v2 state, deltas, history, proposals, executions | admitted once, fleet-wide | no |
| Session-v2 writes | per-session timestamp floor | no |
| Wallet-signed requests, including the nonce | per-signer timestamp floor | no |

What the nonce path still does on every request:

- verifies the session-v2 signature over the envelope, which binds the
  operation and transport, so a nonce signature cannot be used for
  `GetState` or any other operation;
- checks the timestamp window;
- looks up the session in the database on every request (unknown, revoked or
  expired sessions are refused), and the account's current metadata version;
- checks the grant: account, network, Guardian key, current signer set;
- fails closed on any database error.

What it stops doing: inserting the read record, and the read fence's shared
advisory lock that goes with it. The combined lookup that already returns the
state head stays, now read-only.

### Window

Today a session read is valid within 60 s either side of server time, so a
request stamped at the future edge stays usable for about 120 s after it is
signed. For the nonce path the decision is **60 s past, 30 s future**, which
bounds a captured nonce request's validity to about 90 s. SDKs sign session
reads at send time and at most 1 s ahead of their own clock, so only clock
difference counts. A client whose clock runs more than about 29 s fast gets
`timestamp_out_of_window` on every nonce poll; retrying does not help, since
it signs with the same clock, and neither SDK falls back to `get_state`, so
its sync stops. An earlier draft proposed 5 s, which would have stopped sync
for a client only about 4 s fast; independent review flagged it, and 30 s
keeps most of the reduction from 120 s without that risk. Other reads keep
the symmetric 60 s window.

## What a replay exposes

Someone holding a copy of a signed session-v2 nonce request (from a shared
HAR file, a proxy or APM log, a browser extension; TLS still protects it in
transit) can present it repeatedly until its window closes and read:

- the account's current canonical nonce and state commitment, for up to
  about 90 s after the request was signed. That tells them **when** the
  account changes and the nonce value itself (how many transactions it has
  executed); for a private account the chain shows the commitment change but
  not the nonce;
- with canonicalization off (optimistic mode), the stored head runs ahead of
  the chain, so a poll also shows writes Guardian accepted that are not yet
  on chain;
- the commitment can confirm a guess of the full account state, which is not
  practical while the state includes key material.

They cannot:

- read state, deltas, proposals or execution data (different operation,
  different signature, still single-use);
- write, approve or execute anything;
- extend the window or forge a new request without the session key;
- keep using it after the session is revoked or expires, since the session
  is checked on every request.

Today the same copy is usable at most once and only if it reaches the server
before the client's own request. The change is from "once" to "repeatedly for
at most about 90 s". Each replay costs one read-only database call plus the
grant check, no more than a refused copy costs today; per-IP rate limiting
bounds that. If the stored head has no nonce yet, each poll, replayed or not,
also runs the nonce backfill until one write succeeds.

### Why not the wallet nonce

A wallet signature binds account, timestamp and payload but not the
operation (`crates/shared/src/auth_request_message.rs`), and `GET /state` and
`GET /state/nonce` take the same `StateQuery`. A wallet nonce signature is
therefore also a valid `get_state` signature, and relaxing its replay rule
would relax full-state reads. Wallet requests keep their floors.

### Why not other session reads

State, deltas and proposals return private content created after the
original request; replaying them is the confidentiality loss the spec
rejects. Their volume is small (state fetches follow nonce changes), so their
single-use record is affordable.

## Alternatives considered

| Option | Replay rule | Effect on the limit | Why not chosen now |
|---|---|---|---|
| Keep everything, larger instance | once | `xlarge` or `2xlarge` (about $490 to $980/month Multi-AZ) | pays for per-read overhead |
| Async commit, connection-wide | once, but records lost on a crash | +9% alone; with the typed fix about 7,100/s instead of 4,850 (CPU bound) | loses acknowledged replay records, floors and application writes after a crash |
| Async commit for the admission call only, with a restart fence | once (see "Deferred complement") | close to the connection-wide figure for reads | deferred: with this decision, too few recorded reads remain to benefit |
| Batch admissions per replica | once | removes the WAL ceiling, spreads per-call cost | more code; still the best option if this decision is rejected |
| Remove the fence lock, trim partitions | once | not measured; the lock's share is a hypothesis | redesign of a security mechanism; matters little if nonce polls stop writing |
| Per-replica memory record | once per replica | no database write | bound grows with replica count; needs route binding to reach once |
| Window-only for all session reads | repeated within window | largest | rejected by the spec on confidentiality |
| Response encryption | useless to a holder of a copy | no write | separate proposal and contract change |

## Deferred complement: asynchronous commit for recorded reads

If the reads that stay single-use (state, deltas, history, proposals) ever
approach the WAL-flush ceiling, their record can be committed without
waiting for the disk while keeping the single-use rule:

- `PERFORM set_config('synchronous_commit', 'off', true)` inside
  `guardian_admit_session_read` applies to that call's own transaction only
  (checked on Postgres 17: the next transaction on the connection is back
  to `on`; a `SET` clause on the function would not reach the commit).
  Writes, floors and everything else stay durable.
- A crash or failover can lose the last few hundred milliseconds of read
  records. A restart fence closes that: admission refuses a read stamped at
  or before the database start time (`pg_postmaster_start_time()`) plus the
  future part of the read window, so a lost record's request can never be
  admitted again.
- Cost: reads are refused for that future margin after every database
  restart (60 s with today's symmetric window; narrowing the future part
  shortens it). Needs a crash step in `verify-auth-replay` and a restart
  test.

Not adopted now: the SDKs poll the nonce and fetch state only after it
changes, so after this decision recorded reads are a small share of
traffic (about 15/s against 7,100 nonce polls/s in the r4 profile) and far
from the ceiling (about 4,850 commits/s on `db.m7g.large` with the typed
query fix). Revisit if the recorded-read rate is sustained above roughly
1,000/s.

## Consequences

- **Contract:** a duplicate session-v2 nonce request returns the nonce instead
  of `authentication_replay`. No new error code or field. Clients sign each
  poll afresh and retry only `authentication_replay`, so no SDK change is
  needed (checked in both SDKs). Session-v2 has not shipped, so version 2 is
  amended in place.
- **Monitoring:** replays of captured nonce requests become indistinguishable
  from polls; `authentication_replay` on nonce polls, today a signal of a
  copied request, disappears. `guardian_session_reads_total{kind, outcome}`
  counts session reads by admission kind (window-only nonce, recorded) and
  outcome, so the recorded-read rate that would trigger the deferred
  complement is visible: it is exported to CloudWatch (EMF allowlist in
  `infra/observability.tf`, dimensions `kind` and `outcome`) and shown in
  the Grafana dashboard ("Session reads by kind and outcome"). Not adopted: a per-replica in-memory record of
  recent poll keys feeding a duplicate counter, never used to refuse. It
  would restore a partial signal (same replica only) at the cost of memory
  sized to the poll rate; revisit if copied polls become a concern.
  Partition maintenance failing is no longer visible from nonce polls, which
  do not use the partitions; the maintenance timestamp alarm covers it.
- **Docs:** `spec/api.md` (session read replay rule), `docs/CONFIGURATION.md`
  ("Each signed read is admitted once"), and this feature's spec security
  section.
- **Rollback:** server-only; reverting restores single-use nonce polls without
  client changes.

## Verification before rollout

1. `verify-auth-replay`: add a nonce-window variant. `AcceptedAtMostOnce`
   still holds for every other operation; the nonce operation is accepted only
   inside its window, never after revocation or expiry, and never under
   another operation's signature.
2. Tests, HTTP and gRPC, Rust and TypeScript clients:
   - a duplicate nonce request is accepted inside the window, refused outside
     it (including the 30 s future bound);
   - a nonce signature is refused for `GetState` and the other reads;
   - revoked, expired and unknown sessions are refused;
   - two replicas accept the same nonce request;
   - a duplicate `GetState` is still refused;
   - a wallet-signed nonce replay is still refused.
3. Measurements, normal commits: the cost of the per-read log line (done
   locally: about 15% of server CPU at 4,000 polls/s and 2 to 3 times the
   p99, so the nonce poll's span is now `debug`); a local
   A/B of the read-only nonce path; then an AWS A/B (typed queries against
   typed queries plus this change) with stepped load, recording missed polls
   and achieved writes, not only completed requests.

Status (2026-11-05): 1 done (`session-nonce-*` cells, with liveness and
future-bound negative controls). 2 done over HTTP in the server's
integration tests (gRPC shares the resolver; two-replica acceptance follows
from the read-only call and is not a separate test). 3 done: locally the
nonce path costs about a third less Postgres CPU per poll; on AWS
(`db.m7g.large`) the Nov 4 ceiling of about 4,850 polls/s (p95 2.2 s) became
5,000/s (every worker's p95 under 10 ms) and 7,500/s (under 150 ms), with
writes no longer starved, and the database saturates near 10,000/s; on
`db.m7g.xlarge` the database runs 10,000/s at 51% CPU, but that run is not
qualified for latency (one server task saturated). The harness reports
weighted per-worker percentiles, not fleet percentiles. Results: `benchmarks/prod-server/results/2026-11-04-devnet-317-bench.md`.

## Questions for reviewers

1. Is "an attacker holding a copied nonce request learns the account's nonce
   and commitment, and when they change, for at most about 90 s" acceptable,
   for public and private accounts alike (the spec allows no account-type
   exemption)?
2. Is 30 s of future skew right for nonce polls, against 60 s for other reads
   (validity about 120 s) or a tighter bound with an SDK fallback?
3. Is losing the replay signal on nonce polls acceptable without the
   per-replica duplicate counter?

## Review so far (2026-11-05, internal)

Two independent internal reviews answered yes to all three questions:

1. Accepted, naming the leaks: a copied poll returns the current nonce and
   commitment until its window closes or the session ends. For a private
   account the nonce (transaction count) is not on chain, and with
   optimistic canonicalization the head can show writes Guardian accepted
   that are not on chain yet. The envelope keeps the signature from being
   used for any other read.
2. 30 s future is right: the SDKs sign at send time with no fallback, so a
   tighter bound stops sync for a fast clock; a client more than about 29 s
   fast gets `timestamp_out_of_window` on every poll.
3. Ship without the duplicate-poll counter: `guardian_session_reads_total`
   (exported, see Consequences) is the signal for the deferred
   asynchronous-commit threshold; copied polls stay bounded by the window
   and per-IP rate limits.

Findings from those reviews fixed on the branch: the expiry check reads the
current clock instead of the transaction start; a recorded read re-reads
the session after the record and removes the record when refusing; local
stores re-check the session after recording; the counter is exported and
on the dashboard; a wallet nonce replay test; the spec's FR-008, US1 and
SC-002 name the exception; the model checks the serving call's session and
window checks with negative controls. The external security review is
still open.
