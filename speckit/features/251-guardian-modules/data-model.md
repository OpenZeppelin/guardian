# Data Model: Guardian Modules

**Feature**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

Wire messages are defined in [contracts/guardian_module_v1.proto](./contracts/guardian_module_v1.proto). This file covers what the Guardian loads, stores and keeps in memory. Column and type names are indicative until the prototype gates close ([research.md](./research.md)).

## Configuration (loaded at startup, not stored)

### ModuleInstanceConfig

One `[[module]]` entry of the catalog file ([contracts/catalog-config.md](./contracts/catalog-config.md)).

| Field | Rules |
|---|---|
| `id` | Unique in the catalog; `[a-z0-9_-]{1,64}`; used in errors, metrics, operator API |
| `kind` | Must name a registered kind, else startup fails |
| `endpoint` | Remote kinds only: `grpc://` (TLS) or `grpc+insecure://` (plain text, loopback hosts only, never in prod) |
| `scope` | `all`, `default` or `assigned` |
| `hooks` | Non-empty subset of the gate and notify hooks; each must be in the module's `describe` |
| `timeout_ms` | Required, 1 to `GUARDIAN_MODULE_TIMEOUT_MAX_MS` |
| `on_error` | `deny` (default) or `allow` |
| `auth` | Remote kinds; mandatory in prod; secret reference for the bearer token, optional mTLS |
| `params` | Seed for the instance's global params, written to the metadata store the first time the id is seen; JSON object; `$secret` references only for in-process kinds (such an instance's global params stay file-managed); validated by `validate_params` |
| `require_fresh_state_ms` | Optional (M2b) |
| `enabled` | Seed value, read only the first time the id is seen |
| `operator_can_disable` | Default `false` |
| `disclosure` | `none` (default), `summary` or `params` (RFC 0002 section 2.7) |
| `description` | Required unless `disclosure = "none"`; at most 500 characters |
| `client_reason` | `full` (default) or `generic` |
| `dry_run` | Default `true` |

**Catalog**: the ordered list of instances plus its `catalog_hash` (hash of the canonical file content), exported per replica and recorded on each signed delta.

**Startup validation**: every enabled instance is created, described and its stored global params validated; failure of an enabled instance stops a prod start; a disabled instance that fails is logged and stays disabled. Budget checks per hook over `all` and `default` instances, enabled or not. Over-budget stored assignments are reported, not fatal.

## Stored state

### Tables

Module state lives in three tables of its own, never in the account metadata row (research D12). The filesystem backend keeps all of them in one file, `<metadata path>/modules/module_state.json`, replaced atomically, so a write and its history land together.

#### `module_instances`

| Column | Rules |
|---|---|
| `module_id` | Primary key; a catalog instance id |
| `enabled` | Seeded from the catalog on first sight; then changed only through the operator API |
| `global_params` | JSON object; seeded from the catalog on first sight; replaced through `PUT /dashboard/modules/{id}/params` (`policies:write_global`); never `$secret`: an instance whose catalog params hold secret references is file-managed, its row stores `{}` and evaluation reads the params from the catalog, so neither a reference nor a resolved secret is stored |
| `seeded_params` | Catalog params at seeding, for drift reporting |
| `updated_at`, `updated_by` | Last write |

Transitions of `enabled`: `enabled -> disabled` only if the catalog entry has `operator_can_disable = true`; `disabled -> enabled` always, after re-running startup validation. Removing an instance from the catalog keeps its row.

#### `module_account_params`

| Column | Rules |
|---|---|
| `account_id`, `module_id` | Primary key; index on `module_id` |
| `kind` | `layer` (on a `scope = all` instance; evaluated as a second pass, so it only adds restrictions) or `assignment` (a `default` or `assigned` instance, params replace the global ones for that account) |
| `position` | Evaluation order among an account's assignments; 0 for layers |
| `params` | JSON object, validated on its own; never `$secret` |
| `updated_at`, `updated_by` | Last write |

Assignments are created for `default` instances in the same transaction as the claim that creates a new account (RFC 0002 section 2.3, "Registration"), preserved on re-onboarding, and replaced only through `PUT /dashboard/accounts/{id}/modules`; layers through `PUT`/`DELETE /dashboard/accounts/{id}/modules/{module_id}/params`. A row naming an instance no longer in the catalog is orphaned: kept, skipped, reported.

#### `module_params_history`

Append-only: `id` (primary key), `version`, `module_id`, `account_id` (null for global), `previous`, `new`, `operator`, `reason`, `at`. Rows of one write share its `version`.

#### `module_state_version`

One row holding the params version. Every write takes its lock first, increments it, updates its rows and appends the history in one transaction, so writes serialize and the version moves in commit order; replicas key their cache on it. On the filesystem backend all tables are one file, `module_state.json`, replaced atomically.

### Delta additions (both backends)

| Field | Set when | Notes |
|---|---|---|
| `modules` (evaluation trace) | The Guardian signs the delta after modules ran | Dashboard detail only. `catalog_hash`, `params_version`, `failed_open` (module, layer, failure kind; omitted when empty). Allowed evaluations are derived from the catalog, not stored. Prototype: `metadata.modules` |
| `signed_at` | Fixed before the pipeline runs | The accounting clock; repeated in `pending` and `CommitEvent` |
| `submitter` | At signing | Verified signer commitment of the request being acked |
| `proposer` | At signing | From `ProposalContext`; empty when no matching proposal |
| `module_projection` | At signing, for every account, once any instance subscribes to `after_transaction_committed` (M2b) | Stored, never recomputed; a queued candidate without it puts subscribed modules for its account behind the activation fence |

### Proposal storage rule

`submit_delta_proposal` is create-if-absent on both backends: a second create of the same proposal id keeps the first record and its `proposer_id`. `update_delta_proposal` is a separate replace-if-present operation on both backends (the filesystem backend today implements it as a re-submit, which create-if-absent would turn into a no-op), so adding a cosigner signature still updates the stored proposal.

**ProposalContext** (read model): `{ proposer: String, metadata: Json }`, returned by a lookup typed `Result<Option<ProposalContext>>`; `Err` is a storage failure, `Ok(None)` is absence.

### Execution reservation addition (after PR #510)

| Field | Rules |
|---|---|
| `requester` | Signer commitment authenticated when the attempt was created; unchanged by repeated requests for the active attempt; survives restarts |

### ModuleEvent (Postgres only, M2b)

| Field | Rules |
|---|---|
| `event_id` | `= new_commitment`; unique |
| `account_id`, `nonce` | Ordering key per account |
| `prev_commitment`, `new_commitment`, `projection`, `signed_at`, `committed_at`, `submitter`, `proposer` | Copied from the delta at commit |

Written in the same storage write that commits the delta (promotion in candidate mode, the push in optimistic mode). Deleted once every current subscriber of the account's modules acknowledged it, or after `GUARDIAN_MODULE_EVENT_RETENTION`.

### ModuleDelivery (Postgres only, M2b)

| Field | Rules |
|---|---|
| `module_id`, `account_id` | Key |
| `acked_nonce` | Highest acknowledged event nonce; advanced only by the lease holder with its fence token |
| `resync_required` | Set when unacknowledged events are dropped (retention lapse, or removal and return of the instance) |
| `started_at_nonce` | Delivery starts at the latest event when the pair is first seen |
| `gap_resume_from_nonce` | Set by a resync, in the same fenced write that moves `acked_nonce` past the dropped events: one above the last skipped event's nonce. While set, no replica sends an event for the pair, only `NotifyGap`; read in the same statement as `acked_nonce`. Cleared by the lease holder, fenced, once the module acknowledges the Gap (or answers `UNIMPLEMENTED`) |

```
            deliver ok                 retention lapse / dropped events
 delivering ──────────► delivering ──────────────────────────► resync_required
     ▲                                                                │
     │                                     operator resync (reason, audited):
     │                                     cursor moved, gap_resume_from_nonce set
     │                                                                ▼
     └──────────── NotifyGap acknowledged (or UNIMPLEMENTED) ───── gap_pending
```

While the pair is in `resync_required` or `gap_pending` the module is not called for that account and its `on_error` applies. A failed `NotifyGap` is retried with backoff; the pair stays in `gap_pending` meanwhile (RFC 0002 section 3.3, revision 23).

**Activation fence** (derived, not stored): for an account, true while any queued candidate lacks module data. While true, every subscribed module's evaluation for that account counts as an error and its `on_error` applies; it clears when those candidates are promoted or discarded.

## In-memory state (per replica)

### CircuitBreaker (per instance)

```
 closed ── N consecutive failures ──► open ── health probe ok ──► half_open
   ▲                                   ▲                              │
   └────────── next call ok ───────────┴──────── next call fails ─────┘
```

A deny is an answer and resets the failure count. `guardian_module_circuit_open{module}`.

### HealthProbe (per enabled instance)

`Describe` every `GUARDIAN_MODULE_PROBE_INTERVAL_MS` under the instance timeout; sets `guardian_module_up{module}`; a changed `protocol_version` counts as a failure.

## Request assembly (per evaluation, not stored)

| Piece | Source | Built when |
|---|---|---|
| `AccountView before/after` | Account state decode (gate G1) | At least one module will be called |
| `TransactionProjection` (Miden variant `MidenTransaction`) | Transaction summary (gate G2) | At least one module will be called; stored when M2b subscribers exist |
| `pending` | Candidate queue and unacknowledged events in one repeatable-read snapshot (M2b) | Only for modules subscribed to `after_transaction_committed` |
| `params` | The layer being evaluated: global params, then the account layer of an `all` instance as a second pass, or the account's own params for `default`/`assigned` | Per module and layer |
| `submitter` | Request authentication (`ResolvedAccount.signer_commitment`) or the execution reservation | Always |
| `proposer`, `unverified_metadata` | `ProposalContext` | Transaction hooks |
