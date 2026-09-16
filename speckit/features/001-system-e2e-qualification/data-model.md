# Data Model: Miden system qualification

**Feature**: `001-system-e2e-qualification` · **Spec**: [spec.md](./spec.md)

These are the entities the suite operates on. None of them touch the Guardian
wire contract; they are the harness's own vocabulary, shared by both drivers so
that a scenario means the same thing in either language.

## Scenario

A named, independently runnable flow. Defined once in the manifest, executed by
one or both drivers.

| Field | Type | Rules |
|---|---|---|
| `id` | string | Stable, kebab-case, unique. Appears unchanged in local output, CI output and recorded results (FR-001). Never renamed once published; a renamed scenario is a new scenario and an abandoned one. |
| `title` | string | Human-readable. |
| `profile` | `deterministic` \| `live` | FR-002. |
| `image_source` | `built` \| `pulled` | FR-002. Deterministic scenarios may declare either (FR-015b). |
| `sdk` | `rust` \| `typescript` \| `both` | `both` expands into one scenario run per SDK sharing the id, distinguished by the result's `sdk` field. |
| `runtime` | `native` \| `server-side` \| `browser` | Required when `sdk` includes `typescript` (FR-023d). |
| `scheme` | `falcon` \| `ecdsa` \| `n/a` | Homogeneous only. Mixed-scheme accounts are a declared capability gap (FR-017a). |
| `shape` | `1-of-1` \| `2-of-3` \| `3-of-3` \| `n/a` | FR-017. |
| `mode` | `online` \| `offline` \| `n/a` | Offline creation is valid only for guardian migration (FR-021a). |
| `actions` | list | From the FR-019a vocabulary. A scenario names its actions; a generic lifecycle does not imply them (FR-019b). |
| `step_budget` | duration | Worst-case wall time from proposal creation to execution. Compared against the network's historical window (FR-018b). |
| `required` | bool | Whether it counts toward a qualification claim (FR-003c). |

**Validation**: `mode: offline` with an `actions` entry other than guardian
migration is rejected at manifest load, not at run time. `runtime: browser`
with `sdk: rust` is rejected. A scenario with no `actions` is rejected.

## Coverage matrix

The declaration of which scenarios are required, per network and SDK pair.

| Field | Type | Rules |
|---|---|---|
| `network` | `devnet` \| `testnet` | |
| `sdk` | `rust` \| `typescript` | |
| `availability` | `available` \| `unavailable` | With a reason when unavailable (FR-026f). |
| `required_scenarios` | list of scenario ids | |

**Rule**: the matrix is not a cross product (FR-026e). An `unavailable` pair is
declared, not omitted, and its scenarios report environment-blocked.

## Target network

| Field | Type | Notes |
|---|---|---|
| `name` | `devnet` \| `testnet` | |
| `rpc_endpoint` | url | |
| `historical_window` | duration | The budget `step_budget` is checked against (FR-018b). |
| `observed_protocol_version` | string | Recorded per run (FR-026d). |
| `fee_asset` | account id \| `none` | Read from the anchor block. `none` means a zero-fee chain, in which case funding is skipped and recorded as not required (FR-032). |

## Artifact set

What a run qualified. Recorded with every result (FR-025g, FR-007a).

| Field | Type | Notes |
|---|---|---|
| `image_digest` | `sha256:...` | Resolved, never a floating tag. |
| `image_revision` | commit sha | From the image's own metadata in pulled mode; from the ref in built mode (FR-007). |
| `pairing` | `branch` \| `release` \| `published` | FR-025a. |
| `sdk_versions` | map | Package name to version. |
| `sdk_integrity` | map | Package name to integrity hash. Published pairing only. |
| `miden_versions` | map | Resolved dependency versions, for skew detection (FR-025h). |

## Treasury

One per network (FR-026b). Long-lived within one contract pin and one chain
generation only.

| Field | Type | Notes |
|---|---|---|
| `network` | target network | |
| `account_id` | account id | |
| `signing_key` | secret | From a protected environment secret; never logged (FR-027, FR-036). |
| `balance_at_start` | amount | Reported (FR-031). |
| `spend_cap` | amount | Per-run ceiling (FR-033). |

**State transitions**: `usable` → `underfunded` (balance below requirement,
FR-030) → `unusable` (absent, wrong fee asset, or incompatible contract
version, FR-033d). All three are setup outcomes, distinct from scenario
failure (FR-033e).

**Concurrency**: mutations serialize per network (FR-033b) and signing
serializes per key (FR-033c).

## Run account

Ephemeral, one or more per run, never reused (FR-028).

| Field | Type | Notes |
|---|---|---|
| `account_id` | account id | |
| `scheme` | `falcon` \| `ecdsa` | |
| `role` | `principal` \| `cosigner` | |
| `funded_amount` | amount | Minimum the scenario requires (FR-029). |
| `residual` | amount | Swept or accepted per FR-033a. |

Run accounts are not deletable on chain and are explicitly outside the cleanup
guarantee (FR-009, SC-007).

## Scenario result

| Field | Type | Notes |
|---|---|---|
| `scenario_id` | string | |
| `sdk`, `network`, `runtime` | enums | Dimensions the run actually used. |
| `outcome` | `passed` \| `failed` \| `skipped` \| `environment_blocked` | Exactly one (FR-003). |
| `reason` | string | Required for every outcome except `passed`. |
| `classification` | `product` \| `environment` \| `setup` | Required when `failed` (FR-037). Never downgraded to flatter a result (FR-041b). |
| `embedded_retry` | bool | Whether the SDK's own transport may have retried a submission (FR-038a). |
| `duration` | duration | |

## Run result

| Field | Type | Notes |
|---|---|---|
| `run_id` | string | |
| `trigger` | `schedule` \| `dispatch` \| `pull_request` \| `publication` \| `pre_release` | |
| `requested_by` | string | Required for `pull_request` (FR-025l). |
| `artifact_set` | artifact set | |
| `network` | target network | |
| `scenario_results` | list | |
| `conclusion` | `success` \| `failure` | Derived per FR-003a. |
| `qualification_claim` | `full` \| `partial` \| `none` | `full` only when every required matrix entry passed (FR-003d). A filtered run is never `full` (FR-003e). |
| `not_covered` | list | What this result does not speak for (FR-003f). |
| `funding_summary` | object | Start balance, spend, projected remaining runs (FR-031). |

**Invariant**: `conclusion: success` does not imply `qualification_claim: full`.
A run whose only non-passing scenarios are environment-blocked concludes
successfully while claiming less than full coverage. Conflating these is the
failure this model exists to prevent.

## Release record

| Field | Type | Notes |
|---|---|---|
| `ref` | git ref | |
| `artifact_set` | artifact set | |
| `run_results` | list | One per network (FR-026c). Never merged into one verdict. |
| `decision` | `proceeded` \| `held` | |
| `decided_by` | string | Required when proceeding over a non-green run (FR-041a). |

**Rule**: the decision is recorded alongside the result and never amends it
(FR-041c).
