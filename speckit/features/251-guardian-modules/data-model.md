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
| `endpoint` | Remote kinds only; `grpc://`; TLS required except loopback outside prod |
| `scope` | `all`, `default` or `assigned` |
| `hooks` | Non-empty subset of the gate and notify hooks; each must be in the module's `describe` |
| `timeout_ms` | Required, 1 to `GUARDIAN_MODULE_TIMEOUT_MAX_MS` |
| `on_error` | `deny` (default) or `allow` |
| `auth` | Remote kinds; mandatory in prod; secret reference for the bearer token, optional mTLS |
| `params` | JSON object; `$secret` references only for in-process kinds; validated by `validate_params` |
| `account_params` | `scope = all` only; map account id to JSON object, shallow-merged over `params`; no `$secret` |
| `require_fresh_state_ms` | Optional (M2b) |
| `enabled` | Seed value, read only the first time the id is seen |
| `operator_can_disable` | Default `false` |

**Catalog**: the ordered list of instances plus its `catalog_hash` (hash of the canonical file content), exported per replica and recorded on each signed delta.

**Startup validation**: every enabled instance is created, described and its params validated (instance and every `account_params` entry); failure of an enabled instance stops a prod start; a disabled instance that fails is logged and stays disabled. Budget checks per hook over `all` and `default` instances, enabled or not. Over-budget stored assignments are reported, not fatal.

## Stored state

### ModuleInstanceFlag (metadata store, both backends)

| Field | Rules |
|---|---|
| `module_id` | Primary key |
| `enabled` | Written from the catalog seed on first sight, then only through the operator API |
| `updated_at`, `updated_by`, `reason` | From the last operator change |

Transitions: `enabled -> disabled` only if the catalog entry has `operator_can_disable = true`, under `policies:write` or `modules:disable`; `disabled -> enabled` always allowed under `policies:write`, after re-running startup validation. Removing an instance from the catalog keeps its flag.

### AccountModuleAssignment (metadata store, both backends)

| Field | Rules |
|---|---|
| `account_id` | Key |
| `entries` | Ordered list of `{ module_id, params }`; `default` and `assigned` instances only; never an `all` instance |
| `updated_at`, `updated_by`, `reason` | From the last operator change |

Created in the same write that creates a new account, holding the copied `default` instances in catalog order. Replaced only through `PUT /dashboard/accounts/{id}/modules`. Preserved on re-onboarding. Entries naming an id no longer in the catalog are **orphaned**: kept, skipped, reported.

### Delta additions (both backends)

| Field | Set when | Notes |
|---|---|---|
| `module_catalog_hash` | The Guardian signs the delta | Dashboard detail only, not in the client API |
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

```
            deliver ok                    retention lapse / dropped events
 delivering ──────────► delivering ─────────────────────────────► resync_required
     ▲                                                                   │
     └──────────── operator resync (reason, audited) + NotifyGap ────────┘
```

While `resync_required` is set the module is not called for that account and its `on_error` applies.

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
| `TxProjection` | Transaction summary (gate G2) | At least one module will be called; stored when M2b subscribers exist |
| `pending` | Candidate queue and unacknowledged events in one repeatable-read snapshot (M2b) | Only for modules subscribed to `after_transaction_committed` |
| `params` | Assignment or catalog, `account_params` merged | Per module |
| `submitter` | Request authentication (`ResolvedAccount.signer_commitment`) or the execution reservation | Always |
| `proposer`, `unverified_metadata` | `ProposalContext` | Transaction hooks |
