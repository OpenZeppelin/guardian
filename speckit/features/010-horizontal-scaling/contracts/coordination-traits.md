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

```text
trait SessionStore {
    async fn put(&self, realm: Realm, token_digest: [u8;32], subject: Subject,
                 issued_at, expires_at) -> Result<()>;
    async fn get(&self, realm: Realm, token_digest: [u8;32]) -> Result<Option<SessionRecord>>;
    async fn revoke(&self, realm: Realm, token_digest: [u8;32]) -> Result<()>; // idempotent (logout)
    async fn sweep_expired(&self) -> Result<u64>;
}
```

Behavioral contract:
- `get` returns `Some` only when `revoked_at IS NULL AND now() < expires_at`.
- Validity is evaluated against the store's clock (DB clock for Postgres).
- `revoke` is idempotent and visible to all replicas immediately.
- Replaces `DashboardState` session map and `EvmSessionState` session map without
  changing the outcome of `authenticate_session` (permissions still re-resolved
  from the live allowlist at call time).

## `ChallengeStore`

```text
trait ChallengeStore {
    async fn issue(&self, realm: Realm, principal: String, challenge: PendingChallenge) -> Result<()>;
    async fn consume(&self, realm: Realm, signing_digest: [u8;32]) -> Result<Option<PendingChallenge>>; // single-use
    async fn list_for(&self, realm: Realm, principal: &str) -> Result<Vec<PendingChallenge>>;
    async fn sweep_expired(&self) -> Result<u64>;
}
```

**Challenge identity decision**: the challenge primary key is the
**`signing_digest`** (`BYTEA`, 32 bytes) — the exact value the client signs and
returns. This is a deliberate behavior decision, not just a column type: today's
`verify` matches a signed response against a `Vec<PendingChallenge>` per principal
(`dashboard/state.rs:170-288`); keying by `signing_digest` preserves that
matching semantics (the returned signature identifies which challenge it answers)
while making single-use consumption a single keyed atomic write. No surrogate
uuid is introduced. `(realm, principal)` remains an index for `list_for`.

Behavioral contract:
- `consume` atomically marks `consumed_at` and returns the challenge only if it
  was unconsumed and unexpired — a replay on any replica returns `None`
  (FR-003).
- Issue-on-replica-A / consume-on-replica-B succeeds (FR-001).

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
strictly increases on each (re)acquisition. Because cancellation is cooperative
there is a window between losing the lease and the pass actually stopping, so the
processor MUST call `verify_held` (fence/ownership re-check) immediately before any
state-mutating on-chain submission or canonical promotion, and MUST skip that
write if it returns `false`. This closes the abort-window split-brain: even if two
replicas briefly both believe they lead, only the current fence holder can commit
a write. TTL + voluntary abort alone is insufficient — the fence check is the hard
guarantee.

## Selection rule (wiring)

`builder/storage.rs` already chooses the storage backend. The same decision point
selects the coordination family:

- `feature = "postgres"` + `DATABASE_URL` set  => Postgres impls (share the
  storage/metadata pool or a dedicated small pool — decided at implementation).
- filesystem backend  => in-memory impls + `AlwaysLeader`.

Coordination availability therefore can never diverge from where shared state
lives. `AppState` (`builder/state.rs`) carries `Arc<dyn SessionStore>`,
`Arc<dyn ChallengeStore>`, `Arc<dyn LeaderElector>`.

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
per-request DB load and reinforces the connection-pool sizing concern (see
plan.md risks and the runbook). Challenges are touched only during login (low
volume); the per-request cost is the session lookup.
