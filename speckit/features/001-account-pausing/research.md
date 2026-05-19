# Research: Operator-Initiated Per-Account Pause

## Decision 1: Ship pause as a self-contained flag with a single chokepoint, not as a `Policy` impl

- **Decision**: The pause runtime is a pair of columns on
  `account_metadata` (`paused_at`, `paused_reason`) plus a single
  helper `services::account_status::ensure_account_active(state,
  account_id)`. Mutating service entry points call the helper
  before any state mutation. There is no `Policy` trait, no
  `PolicyEngine`, no policy registration seam in this feature.
- **Rationale**:
  - **`#181` is shippable inside its own scope.** The architecture
    document frames pause as a built-in `Policy` on a future
    `PolicyEngine` (#182). Building the engine to host one policy
    inverts the cost-value ratio: pause becomes a 2,000-line
    framework PR before a single account can be paused.
  - **The external contract is identical either way.** Pause
    endpoints, the persisted columns, the error code, the response
    `details`, and the audit shape are all owned by this feature
    and do not change between "flag with helper" and "first
    built-in policy on a `PolicyEngine`". The decision is purely
    internal.
  - **Forward-swappability is testable.** The single-chokepoint
    invariant (FR-025) plus the integration test that drives all
    three mutating paths against a paused account is a structural
    guarantee that the helper is the only call site. When #182
    lands, replacing the helper body with
    `policy_engine.evaluate_all(account_id,
    PolicyAction::ApplyDelta, &ctx)` (and equivalents for
    `ProposalSubmit`, `ExecuteProposal`) is a localized refactor.
    SC-007 verifies at #182 merge time.
- **Alternatives considered**:
  - *Ship `PolicyEngine` now and AccountPaused as its first
    policy*. Rejected: drags in policy CRUD endpoints
    (`policies:write` gate), atomic-snapshot semantics, action-
    kind filtering, and the two evaluation points
    (`ProposalSubmit` + `ApplyDelta`) — all of which are
    `#182`'s scope and would balloon this feature past its
    operational purpose.
  - *Scatter `paused_at` checks across each mutating handler
    inline*. Rejected: violates FR-025; makes the future
    `PolicyEngine` migration a multi-site refactor; risks one
    mutating path being missed and silently allowing writes on a
    paused account (a security hole).
  - *Implement pause as an in-memory `Mutex<HashSet<AccountId>>`
    with a rebuild-from-DB on startup*. Rejected: complicates the
    consistency story (FR-017 — pause read-after-write), adds a
    second source of truth, and does not survive multi-server
    deployments. The DB column is the source of truth.

## Decision 2: First-writer-wins for the forensic timestamp on re-pause

- **Decision**: A pause request issued against an already-paused
  account returns 200 success but does **not** overwrite the
  existing `paused_at` or `paused_reason`. The original
  forensic timestamp + reason are preserved. The response body
  carries the **existing** `paused_at` and `paused_reason` plus
  `before_state == after_state == "paused"`. An `admin_actions`
  audit row is still written so the attempt is attributable
  (FR-019).
- **Rationale**:
  - The original timestamp carries forensic weight: it is the
    moment Guardian first refused mutating activity on that
    account. Operators chasing an incident need to know that
    timestamp, not a later one from a retry.
  - Mid-incident, multiple on-call operators may issue the
    pause near-simultaneously (one via the dashboard, one via
    a script). Each retry overwriting the timestamp would
    obscure when the gate actually closed.
  - The "amend the reason" use case is rare. Operators who
    want to update the reason can unpause + re-pause; the audit
    trail captures both transitions and the timeline is
    explicit.
- **Implementation**: Postgres uses `UPDATE account_metadata SET
  paused_at = COALESCE(paused_at, $1), paused_reason =
  COALESCE(paused_reason, $2) WHERE account_id = $3 RETURNING
  ...`. `COALESCE` makes the operation a no-op on the columns
  when they are already set. Filesystem backend mirrors with an
  in-memory equivalent under the existing file lock.
- **Alternatives considered**:
  - *Last-write-wins (overwrite timestamp + reason on re-pause)*.
    Rejected: erases forensic context (see rationale).
  - *Reject re-pause with `GuardianError::AccountAlreadyPaused`*.
    Rejected: makes pause non-idempotent, forces operators to
    perform a pre-check (`GET /accounts/{id}` then conditionally
    POST), and races on concurrent operators.
  - *Append a list of pause events on the row*. Rejected:
    duplicates the audit trail in operational state; bloats the
    metadata table; complicates the response shape.

## Decision 3: Use `/dashboard/accounts/{id}/pause` path, not the arch-doc-suggested `/v1/operator/accounts/{id}/pause`

- **Decision**: Endpoint paths are
  `POST /dashboard/accounts/{account_id}/pause` and
  `POST /dashboard/accounts/{account_id}/unpause`. The pause
  state appears on the **existing** `GET /dashboard/accounts/{account_id}`
  detail endpoint as two new nullable fields.
- **Rationale**:
  - The architecture document predates the operator dashboard
    surface that `005-operator-dashboard-metrics` shipped.
    Guardian's operator HTTP surface is **rooted at
    `/dashboard`**, not `/v1/operator`. Account-detail today is
    `GET /dashboard/accounts/{id}` (see
    `005-operator-dashboard-metrics` plan §Server — HTTP Surface).
  - Splitting pause into a parallel `/v1/operator/*` mount would
    fork the operator surface, double the documentation and the
    operator-client wrappers, and break the existing
    `005-operator-dashboard-metrics` URL contract.
  - Existing operator authz machinery (`authz::enforce` middleware,
    permission registration, audit emission with `OriginalUri`)
    is already wired under `/dashboard` and Just Works for these
    routes — adopting it costs zero new infrastructure.
- **Alternatives considered**:
  - *Match the arch doc literally with `/v1/operator/...`*.
    Rejected: forks the operator surface, breaks the URL
    convention of `005-operator-dashboard-metrics`, and is
    cosmetic — the arch doc's value is in the design decisions
    (server-side, single-chokepoint, audited), not the URL
    prefix.
  - *Use a separate subpath like `/dashboard/pause/{id}`*.
    Rejected: hides the account-scoping in the URL, complicates
    REST cardinality (the resource is the account; pause is an
    action on it).

## Decision 4: Extend `account_metadata` table with two nullable columns; do not introduce a separate `account_pause` table

- **Decision**: Add `paused_at TIMESTAMPTZ NULL` and
  `paused_reason TEXT NULL` columns directly to
  `account_metadata`. No separate pause table; no pause history
  table (the `admin_actions` audit log is the history).
- **Rationale**:
  - Pause is a per-account property that is read on the hot
    mutating path. The chokepoint helper executes one metadata
    read per `push_delta` / `push_delta_proposal` /
    `sign_delta_proposal` call. Colocating with
    `account_metadata` keeps that read to a single row fetch
    that may already be in the row cache from a preceding
    handler-side load.
  - A separate `account_pause` table requires either an `INNER
    JOIN` on the hot path (one round trip, two index seeks) or a
    second separate query (one round trip, but two seeks
    instead of one). Both options are net slower than column
    extension on a row already touched by the request.
  - The two new columns are NULL for active accounts (the
    majority case in steady state), so the row width cost is
    bounded; `TIMESTAMPTZ` is 8 bytes when present, `TEXT NULL`
    has near-zero overhead when null.
  - Forensic history lives in `admin_actions` (the append-only
    audit log) — duplicating it in a `account_pause_history`
    table would split the source of truth.
- **Implementation**: migration adds the two columns and a
  partial index `WHERE paused_at IS NOT NULL` so "list all
  currently-paused accounts" (operational query) is cheap even
  on a wide table. The partial index keeps the index size
  proportional to the count of currently-paused accounts, not
  total accounts.
- **Alternatives considered**:
  - *Separate `account_pause` table with `account_id PK,
    paused_at, paused_reason, paused_by`*. Rejected: hot-path
    join cost; second source of truth; no value-add over the
    flat columns + `admin_actions` audit log.
  - *Pause-history table `account_pause_events(account_id,
    transition, at, by, reason)`*. Rejected: duplicates
    `admin_actions`.

## Decision 5: Enforce the single-chokepoint invariant by convention + integration test, not by static analysis

- **Decision**: FR-025 (mutating handlers MUST NOT read
  `paused_at` directly) is enforced by:
  (a) a single chokepoint module `services::account_status` that
      exposes one helper; mutating services consume the helper.
  (b) a code-review checklist item that flags any new direct
      read of `paused_at` outside `account_status` and the
      pause/unpause services and the read-snapshot projection.
  (c) an integration test
      `crates/server/tests/account_pause_chokepoint.rs` that
      pauses an account and drives all three mutating paths
      (push_delta, push_delta_proposal, sign_delta_proposal) on
      both gRPC and HTTP transports, asserting every path
      rejects with `GUARDIAN_ACCOUNT_PAUSED`.
- **Rationale**:
  - **A static check (clippy lint, `#[deny]` attribute, custom
    AST analyzer) is disproportionate to the risk.** There are
    three mutating paths and they live in adjacent files; a
    code review catches any new read trivially.
  - **The integration test is the load-bearing guarantee.**
    If a new mutating path is added that bypasses the
    chokepoint, the test fails when that path is exercised.
    The test scales: each new mutating path adds a sub-test
    with the same setup.
  - **A macro-based "register your mutating path here" registry
    was considered**. Rejected — it adds boilerplate to a
    well-understood call surface (three functions), and the
    chokepoint helper is sufficient by itself.
- **Trade-off**: a future developer can add a fourth mutating
  path and forget the chokepoint. The integration test, plus the
  small surface area (three mutating service entry points all
  in one directory), makes this an acceptable risk. When `#182`
  lands and the helper is replaced by a `PolicyEngine`, the
  engine becomes the registry by construction — the registry
  problem solves itself.

## Decision 6: gRPC parity divergence on pause/unpause **control**, full parity on **enforcement**

- **Decision**:
  - **Control endpoints** (`POST /pause`, `POST /unpause`) are
    HTTP-only. The dashboard surface where they live is HTTP-only
    by precedent (`005-operator-dashboard-metrics` Decision 2).
  - **Enforcement** (the chokepoint helper) applies on **both**
    gRPC and HTTP mutating callers identically. Multisig SDKs
    (gRPC) and EVM clients (HTTP) reach the same
    `services::push_delta` / `push_delta_proposal` /
    `sign_delta_proposal` functions, so the chokepoint catches
    them both.
  - `GuardianError::AccountPaused` maps consistently across
    transports: HTTP `409 Conflict` body carries `code =
    "GUARDIAN_ACCOUNT_PAUSED"` + `paused_at` + `paused_reason`;
    gRPC `Status::failed_precondition` carries the same fields
    via the existing `Status::with_details` pattern.
- **Rationale**:
  - The operator dashboard is HTTP-only by deliberate design,
    not accident. Mirroring pause to gRPC would force a new
    gRPC service for operator-only RPCs, expanding scope.
  - The pause **purpose** — stop new mutating work — is fully
    served by enforcing at the service layer (below the
    transport split). Both transports honor it.
- **Constitution §II compliance**: documented divergence in the
  Constitution Check table of `plan.md`. Matches the precedent.
- **Alternatives considered**:
  - *Mirror pause/unpause as a gRPC `OperatorService` RPC*.
    Rejected: new gRPC service surface; new TS wire types in
    `@openzeppelin/guardian-client`; no operational need (operators
    drive Guardian over HTTP).
