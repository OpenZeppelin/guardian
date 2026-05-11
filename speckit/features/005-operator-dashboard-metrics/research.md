# Research: Operator Dashboard Metrics — Pagination, Info, and Activity

## Decision 1: Promote `status_kind` and `status_timestamp` to typed indexed columns; push pagination, sort, and filter into SQL

- **Decision**: Add a Phase A schema migration that promotes two
  derived fields from the existing `status` `Jsonb` blob into typed,
  indexed columns on both `deltas` and `delta_proposals`:
  `status_kind text NOT NULL` (one of `pending` / `candidate` /
  `canonical` / `discarded`) and `status_timestamp timestamptz NOT
  NULL`. Backfill both columns from the existing rows (`status->>'status'`
  and `(status->>'timestamp')::timestamptz`). Add composite indexes
  shaped to the dashboard sort keys: `(status_kind, status_timestamp
  DESC, account_id, id)` for global delta feeds and
  `(status_kind, status_timestamp DESC, account_id, id)` on
  `delta_proposals` for global proposal feeds. Per-account endpoints
  continue to sort by the immutable primary key (`delta.id DESC`,
  `delta_proposal.id DESC`) so per-account cursors remain fully
  stable. The Postgres backend pushes pagination, status filtering,
  and sort entirely into SQL via these columns.
- **Rationale**:
  - **Service-layer fan-out is the wrong shape for the global feeds.**
    The original "no migration, sort by PK" plan forced the service
    layer to load every account's deltas into memory and sort across
    them at request time, which scales poorly past low-hundreds of
    accounts and pushes filtering work into Rust that the database
    already does well.
  - **Composite indexes give the global sort for free.** With
    `(status_kind, status_timestamp DESC, account_id, id)` in place,
    `WHERE status_kind = ANY($1)` + `ORDER BY status_timestamp DESC,
    account_id ASC, id ASC LIMIT n` is an index range scan; pagination
    via composite cursor predicates becomes a single seek.
  - **Per-account cursor stability is preserved.** Per-account
    endpoints still sort by the immutable PK, so they retain the "skip
    /repeat under concurrent sort-key updates" guarantee. Only the
    global feeds sort on `status_timestamp`, and the FR-005 caveat is
    scoped to those two feeds (and to `/dashboard/accounts`, which
    sorts on `updated_at`).
  - **Backfill is mandatory.** Existing Guardian deployments already
    have populated `delta_proposals` and `deltas` tables; the
    migration's `up.sql` runs the `UPDATE … SET status_kind =
    status->>'status'` (and the timestamp parse) so the new columns
    arrive consistent with history.
  - **Filesystem behavior unchanged.** The filesystem backend keeps
    the fan-out implementation but is bounded by
    `filesystem_aggregate_threshold` (Decision 5) so the degraded path
    is honest.
- **Migration shape**:
  - `up.sql`: `ALTER TABLE` adds the two columns nullable, backfills
    from `status` Jsonb, then `SET NOT NULL`. Adds a `CHECK
    (status_kind IN ('pending','candidate','canonical','discarded'))`
    on each table. Creates the composite indexes.
  - `down.sql`: `IF EXISTS` clauses drop indexes, the check
    constraint, and the columns. Safe to rerun.
  - Postgres dual-write: every `INSERT`/`UPDATE` path that writes
    `status` also writes `status_kind` and `status_timestamp` via a
    `derive_status_columns(&DeltaStatus)` helper, so the Jsonb blob
    and the typed columns can never drift.
- **Alternatives considered**:
  - "No migration, sort by PK" (the original Decision 1) — rejected
    because it forced service-layer fan-out for the global feeds and
    moved filter/sort/pagination work out of the database.
  - Add only an expression index on `(status->>'status')` — rejected
    because composite indexes on derived expressions are awkward to
    maintain, and the typed columns make Diesel ergonomics far
    better.
  - Keep mutable `status_timestamp` for per-account endpoints —
    rejected; the immutable-PK sort is strictly better for per-account
    cursor stability and the typed columns are only needed where the
    sort key is `status_timestamp`.

When this decision should be revisited: if a future feature needs
sub-status fields (e.g., a `retry_count` index for retry analytics) or
sub-sort orderings beyond `status_timestamp`, extend the migration
rather than adding a parallel index family on the Jsonb blob.

## Decision 2: Dashboard surface is HTTP-only for v1

- **Decision**: All new endpoints land on the dashboard HTTP surface
  only. No gRPC equivalents are added in this feature.
- **Rationale**: The dashboard UI is the single intended consumer in v1,
  and `004-operator-http-client` already establishes that the typed
  consumer wrapper lives in the TypeScript `guardian-operator-client`.
  Adding gRPC duplicates the surface area without a real consumer and
  would also require routing the cursor codec and error taxonomy through
  protobuf, which is more work than v1 warrants.
- **Alternatives considered**:
  - Mirror every dashboard route on gRPC for parity — rejected; spec
    FR-027 explicitly limits scope, and Constitution §II is satisfied
    by a documented divergence.
  - Add gRPC for the global feeds only — rejected; introduces a partial
    parity matrix that is harder to reason about than full HTTP-only.

## Decision 3: Account list endpoint is breaking, not dual-mode

- **Decision**: Replace `003-operator-account-apis`' unparameterized
  full-inventory `GET /dashboard/accounts` with a single, always-
  paginated endpoint. Remove `total_count` from the list response;
  aggregate counts live only on `/dashboard/info`. Update the internal
  dashboard UI consumer in the same change set.
- **Rationale**: A dual-mode endpoint (unparameterized → full inventory,
  parameterized → page) creates an internal-contract quirk that leaks
  onto every other paginated endpoint via "same policy as the list"
  references, and it forces a confusing decision tree on every test and
  every TS wrapper. The internal consumers are owned by this team and
  can be updated atomically.
- **Alternatives considered**:
  - Preserve unparameterized behavior with `limit` opt-in — rejected
    after spec review pass #2 because of the reviewer-flagged FR-002 vs
    FR-007 contradiction and the inability to express a clean
    "same-policy-as-list" rule for the rest of the endpoints.
  - Soft-deprecate via `Deprecation` headers and dual-mode for one
    release — rejected; the only consumer is internal, so the soft
    deprecation buys nothing and adds a release of carrying two contracts.

## Decision 4: Cursor codec is opaque base64url HMAC-signed; no TTL in v1

- **Decision**: Cursors are opaque base64url-encoded payloads of the
  form `{kind, sort_keys..., tiebreaker_id}`, HMAC-signed with a server
  secret read from the existing dashboard config block. They are
  validated for tampering and for reference to data that still exists.
  No expiry is encoded.
- **Rationale**: HMAC signing is the simplest tamper-evident
  representation that does not require server-side cursor state, which
  matches Guardian's stateless dashboard model. Skipping a TTL keeps the
  v1 contract small; cursors are short-lived in practice (single-page
  navigation) and the data-no-longer-exists rejection covers the only
  meaningful staleness case. Any future TTL can be added in a follow-up
  feature without breaking the cursor envelope.
- **Operational note**: rotating the HMAC secret requires accepting both
  the previous and current secret for one release window, otherwise
  in-flight cursors break across deploys. This is captured in the plan's
  storage workstream.
- **Alternatives considered**:
  - Server-side cursor state in Redis — rejected; introduces a new
    dependency for what is fundamentally stateless.
  - JWT cursors — rejected; oversized for the payload, and the JWT
    typing layer adds nothing over a hand-rolled HMAC envelope.

## Decision 5: Filesystem backend may degrade cross-account aggregates

- **Decision**: Above a configurable inventory-size threshold (default
  1,000 accounts; server config field `filesystem_aggregate_threshold`),
  the filesystem backend MAY return a degraded marker on the info
  endpoint cross-account aggregates and on the global feeds rather than
  perform a full filesystem scan. Per-account endpoints behave
  identically on both backends.
- **Rationale**: The filesystem backend stores deltas per-account
  directory and has no global index. Honoring a strict cross-account
  parity contract would require fanning out a walk over every account
  on every aggregate request — operationally fine at small inventory
  sizes but linear in account count past the threshold. Postgres is
  the production backend; the filesystem backend's role is local dev
  and small-scale ops, where 1,000 is comfortably above realistic
  workloads. Above that, a degraded marker is more honest than a slow
  full scan.
- **Alternatives considered**:
  - Block the feature on building a global filesystem index — rejected;
    out of proportion to the dev-mode use case and unnecessary on
    Postgres.
  - Always full-scan and accept the latency — rejected; would silently
    degrade the dashboard at scale on the wrong backend.

## Decision 6: Two endpoints for per-account history (deltas + proposals), not one

- **Decision**: Per-account history is exposed as two distinct
  endpoints — `/dashboard/accounts/{id}/deltas` and
  `/dashboard/accounts/{id}/proposals` — with separate envelopes and
  separate per-account record-identifier schemes (`delta.nonce` vs
  `delta_proposal.commitment`).
- **Rationale**: Deltas (`deltas` table, statuses
  `candidate`/`canonical`/`discarded`) and proposals (`delta_proposals`
  table, status `Pending` only) are two distinct state machines.
  Cramming them into one feed forces a discriminator field, an
  awkward shared identifier scheme, and a confusing "what status does a
  proposal have" question. The two-endpoint split also lets single-key
  Miden accounts and EVM accounts return an empty proposal queue
  cleanly without polluting their delta history.
- **Alternatives considered**:
  - Single combined activity endpoint with discriminator — rejected per
    spec review #1.
  - Deltas-only with proposal-stage detail embedded — rejected; would
    re-conflate the state machines and make the EVM/single-key empty
    case awkward to express.

## Decision 7: `latest_activity` derives from delta + proposal `status_timestamp` columns

- **Decision**: `latest_activity` on `/dashboard/info` is computed as
  the greater of `MAX(status_timestamp)` across the `deltas` table and
  `MAX(status_timestamp)` across the `delta_proposals` table. Both
  columns are the typed `timestamptz` promoted by the Phase A
  migration (Decision 1).
- **Rationale**: Operators consider both new proposals and delta
  state-transitions as "activity". Defining `latest_activity` as
  delta-only would understate liveness on accounts whose multisig
  signing is in progress but where no delta has yet been committed.
  Two indexed `MAX` queries against the typed columns return in single
  digits of milliseconds even at large inventory sizes.
- **Alternatives considered**:
  - Delta status timestamp only — rejected for the reason above.
  - Last metadata `updated_at` — rejected because metadata changes for
    administrative reasons unrelated to user-visible activity.
  - `Jsonb` extraction on the `status` blob (the original framing) —
    superseded by Decision 1's typed columns; the typed `MAX` is both
    cheaper and indexable.
  - Approximate `MAX(id)` instead of timestamp — rejected because the
    info response already exposes `latest_activity` as a wall-clock
    timestamp string for display, not as a counter.

## Decision 8: v1 is Miden-oriented; EVM accounts excluded from proposal endpoints

- **Decision**: EVM accounts (`Auth::EvmEcdsa`) always return an empty
  paginated result on `/dashboard/accounts/{id}/proposals` and never
  appear in `/dashboard/proposals` in v1. The info response does not
  enumerate networks. v1 targets Miden deployments.
- **Rationale**: A Guardian instance is gated to one network family in
  practice — the default build serves Miden, and EVM support sits
  behind the `evm` server feature flag with its own separate proposal
  storage path that does not flow through `delta_proposals`. Surfacing
  EVM proposals on the dashboard requires its own data-shape decision
  paired with EVM-specific dashboard work and is out of scope for v1.
  Per-network counts on the info response would also be redundant for
  the typical Miden-only deployment (the dashboard already knows its
  own deployment context).
- **Alternatives considered**:
  - Embed EVM proposals on the same endpoint with a different per-entry
    shape — rejected; conflates two different proposal models behind
    one envelope.
  - Add a separate `/dashboard/evm/proposals` endpoint — deferred to a
    follow-up feature paired with the EVM-specific dashboard work.

## Deferred Topics

- Per-cosigner identity list on proposal entries (FR-020).
- `transaction_type` field on delta entries (out of scope until a
  stable cross-network derivation rule exists).
- Asset / balance / token-amount / TVL data surface (FR-024); requires
  either a `state_json` schema convention or a network-specific
  account-inspector extension.
- Cursor TTL / explicit expiry semantics.
- gRPC parity for the dashboard endpoints.
- A unified "not yet canonical" view across deltas + proposals.
- Read-side rate limiting and audit logs.
