# Contract: Coordination Traits

**Feature**: 010-horizontal-scaling

These are the **internal** server traits introduced by this feature. They are not
part of any client (HTTP/gRPC) wire contract — no proto, payload, status enum, or
error surface changes. The traits exist so that shared coordination has two
interchangeable implementations selected with the storage backend:

| Trait | In-memory impl (filesystem/dev) | Postgres impl (prod) | Backs table |
|---|---|---|---|
| `SessionStore` | `InMemorySessionStore` | `PgSessionStore` | `auth_sessions` |
| `ChallengeStore` | `InMemoryChallengeStore` | `PgChallengeStore` | `auth_challenges` |
| `LeaderElector` | `AlwaysLeader` | `PgLeaseElector` | `worker_leases` |

All methods are `async` and return the crate's existing error type; auth-facing
errors MUST map to the **same** boundary errors operators/clients see today
(Constitution IV — no error-surface drift).

## `SessionStore`

Each store instance is **realm-bound at construction** (operator vs evm), so the
methods carry no realm. `StoredSession { subject: SessionSubject, issued_at,
expires_at }`; `SessionSubject` is `Operator { operator_id, commitment }` |
`Evm { address }` (no permissions — re-resolved per request).

```text
trait SessionStore {
    async fn insert(&self, key: [u8;32], session: StoredSession) -> Result<()>;
    async fn get(&self, key: &[u8;32], now) -> Result<Option<StoredSession>>;
    async fn revoke(&self, key: &[u8;32]) -> Result<Option<StoredSession>>; // logout; returns prior for logging
    async fn sweep_expired(&self, now) -> Result<u64>;
}
```

Behavioral contract:
- `get` returns `Some` only when the session is unrevoked and `now < expires_at`.
- Validity is evaluated against the store's clock (DB clock for Postgres).
- `revoke` returns the prior session (for logout logging) and, once revoked, `get`
  MUST reject it on every replica until natural expiry. The Postgres impl marks
  `revoked_at` and keeps the row until expiry; the in-memory impl removes it.
- Replaces the `DashboardState` and `EvmSessionState` session maps without
  changing the outcome of `authenticate_session` (permissions still re-resolved
  from the live allowlist at call time).

## `ChallengeStore`

Each store instance is **realm-scoped** at construction (operator vs evm), so the
trait methods don't take a realm. The stored challenge carries a realm-appropriate
`key` and `payload`:

```text
trait ChallengeStore {
    async fn issue(&self, principal: &str, challenge: StoredChallenge, max_outstanding: usize, now) -> Result<()>;
    async fn active_for(&self, principal: &str, now) -> Result<Vec<StoredChallenge>>;
    async fn consume(&self, principal: &str, key: &str, now) -> Result<bool>; // true => this caller won the single-use claim
    async fn sweep_expired(&self, now) -> Result<u64>;
}
struct StoredChallenge { key: String, payload: ChallengePayload, issued_at, expires_at }
enum ChallengePayload { OperatorDigest(Word), EvmChallenge { address, nonce, issued_at, expires_at } }
```

**Why match-in-Rust, not match-in-store**: the two realms verify differently and
neither check is expressible in SQL — operator does a Falcon
`public_key.verify(signing_digest, sig)` (`dashboard/state.rs:228-230`), EVM does
a nonce compare then `recover_session_address(challenge, sig)`
(`evm/session.rs:112-127`). So the store returns candidate payloads
(`active_for`), the caller matches one, then `consume(principal, key)` atomically
claims it. `key` is the signing-digest hex (operator) or the nonce (EVM); see
data-model.md table `auth_challenges` `(realm, challenge_key)`.

Behavioral contract:
- `consume(principal, key)` atomically sets `consumed_at` and returns `true` only
  if the challenge was unconsumed and unexpired — a replay (or a lost race) on any
  replica returns `false` (FR-003).
- Issue-on-replica-A / match+consume-on-replica-B succeeds (FR-001).

## `LeaderElector`

```text
trait LeaderElector {
    async fn try_acquire(&self, lease: &str, holder_id: &str, ttl: Duration) -> Result<Option<Lease>>;
    async fn renew(&self, lease: &Lease) -> Result<bool>;     // false => lease lost
    async fn verify_held(&self, lease: &Lease) -> Result<bool>; // fence-checked ownership at submission boundary
    async fn release(&self, lease: Lease) -> Result<()>;       // graceful shutdown
}
struct Lease { name, holder_id, fence_token, expires_at }
```

Behavioral contract:
- At most one holder satisfies `renew` at any instant (atomic conditional write +
  DB-clock TTL).
- `AlwaysLeader` always returns a lease, always renews `true`, and `verify_held`
  always returns `true` (single replica).

**Renewal concurrency (resolves the long-pass split-brain)**: lease renewal MUST
run on its **own timer** (`renew_interval`, e.g. 5s) in a task **concurrent with**
the canonicalization pass — NOT at tick boundaries. A pass may run longer than the
check interval; renewal at tick boundaries would let the lease expire mid-pass
while a renewal was still pending, allowing another replica to claim it. The
worker therefore becomes: one renewal task + the pass, sharing a cancellation
signal (e.g. `tokio_util::sync::CancellationToken` or a `watch` channel).

**Cooperative cancellation (makes "abort the current pass" mechanical)**: today
`process_all_accounts()` is a single awaited call with no cancellation hook
(`worker.rs:27-44`). This feature adds a cancellation check that the processor
polls **between accounts** (and before each on-chain submission). When `renew`
returns `false`, the renewal task trips the cancellation signal and the pass stops
at the next checkpoint. "Abort the current pass" is thus a concrete mechanism, not
just a requirement.

**Fence enforcement at the submission boundary (MUST, not may)**: `fence_token`
advances on each change of holder (a steal). Because cancellation is cooperative
there is a window between losing the lease and the pass actually stopping, so the
processor MUST call `verify_held` (fence/ownership re-check) immediately before
**every** state-mutating write — canonical `submit_state`/`submit_delta` **and**
the retry/discard writes — and MUST skip the write if it returns `false`.

The fence is **advisory**, not atomic: `verify_held` is a separate round-trip, so
in principle the lease could be stolen between the check and the write (TOCTOU).
This is acceptable because the writes are **idempotent** — canonical promotion is
a deterministic upsert (same delta → identical bytes) and retry/discard are
idempotent per candidate — so a brief two-leader overlap can at most re-apply the
same transition, never corrupt state. The fence + idempotency + cooperative
cancellation together strongly mitigate split-brain; TTL + voluntary abort alone
is not relied on.

## Selection rule (wiring)

`builder/storage.rs` already chooses the storage backend. The same decision point
selects the coordination family, keying on the **storage backend alone**:

- `feature = "postgres"` + `DATABASE_URL` set => Postgres impls (share the
  storage/metadata pool or a dedicated small pool — decided at implementation).
- filesystem backend => in-memory impls + `AlwaysLeader`.

Coordination is **not** gated on `GUARDIAN_MAX_REPLICAS` or any other tunable:
a Postgres deployment always uses shared coordination. This is deliberate and
default-safe — a missing/mis-set tunable must never silently revert a multi-replica
deployment to per-process state (the #242 bug). The single-instance
session-lookup optimization is deferred to a future explicit, guarded opt-in.

Coordination availability therefore can never diverge from where shared state
lives. Because the session/challenge stores are realm-bound, they are owned by
their realm's consumer, not shared on `AppState`: the builder constructs an
operator-realm `SessionStore`+`ChallengeStore` pair injected into `DashboardState`
and an evm-realm pair injected into `EvmSessionState`. `AppState`
(`builder/state.rs`) carries only `Arc<dyn LeaderElector>` (used by the
canonicalization worker).

## Availability & performance trade-offs (explicit behavior changes)

These are deliberate consequences of moving auth state into Postgres. They are
behavior changes from today's always-available in-memory maps and are stated here
so they are not surprises.

**Shared-store outage => auth fails closed**: with the Postgres impls, a `get`,
`consume`, `issue`, or `put` that errors because Postgres is briefly unavailable
results in the authenticated request / login being **rejected** (fail-closed), not
allowed through. This is the safe choice for a custody system: a DB blip must
never grant access. It is a change from today, where the in-memory map is always
available and never fails for store reasons. The boundary error returned MUST stay
within the existing auth/transient-error surface (no new error shape); operators
see auth failures during a DB outage, which is expected and documented in the
runbook. The canonicalization lease likewise fails closed: a renewal that errors
is treated as a lost lease (the holder steps down), so an outage stalls
canonicalization rather than risking double-processing — it resumes when the DB
returns.

**Per-request DB lookup is a deliberate trade-off vs. caching**: FR-003 requires
logout/expiry to be honored on **every** replica **immediately**, which rules out
a local per-replica session cache (a cache would serve revoked sessions until its
TTL). The accepted consequence is that every authenticated request performs one
indexed Postgres `SELECT` (by `token_digest` PK) where today it is an in-memory
map hit. Immediate revocation is chosen over lower per-request latency. This adds
per-request DB load and reinforces the connection-pool sizing concern (see the
horizontal-scaling runbook). Challenges are touched only during login (low
volume); the per-request cost is the session lookup.
