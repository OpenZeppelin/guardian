---

description: "Task list for feature 007 — dashboard delta activity feed and detail view"
---

# Tasks: Dashboard delta activity feed and detail view

**Input**: Design documents from `/Users/zeljkomarkovic/Documents/Projects/guardian/speckit/features/007-dashboard-delta-details/`
**Prerequisites**: plan.md ✅, spec.md ✅, research.md ✅, data-model.md ✅, contracts/ ✅, quickstart.md ✅

> **⚠ ARCHITECTURE REVISION 2026-05-25** — this task list was authored against the original "read-time decode, no schema migration" design. The actual implementation pivoted to "push-time derivation with a new `metadata JSONB` column" after the original design was found to lose multisig `proposal_type` (the TS client unwraps the proposal payload before calling `pushDelta`). T001–T018 below correspond to the original Phase 1–3 (foundational `delta_summary` module + listing enrichment); the *outputs* of those tasks were rewritten in the 2026-05-25 pivot, but the original phasing still loosely describes what was done. **Net status**: every task in the MVP scope (T001–T018) is complete, just with the new architecture, plus an additional set of post-pivot tasks (migration, push-time pipeline, typed structs, integration tests) tracked informally. See plan.md banner + research.md Decisions 2/3 for the authoritative design.

**Tests**: Not requested as TDD. Each implementation task includes its inline `#[cfg(test)] mod tests` coverage per the repo convention (no separate `tests/` directory); the Validation Matrix in `plan.md` maps every FR/SC to a concrete test.

**Organization**: One phase per user story so each story can be shipped independently. Foundational `delta_summary` module is shared across all stories.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: US1 / US2 / US3 (Setup, Foundational, and Polish have no story label)
- All paths are absolute or repo-root-relative

---

## Phase 1: Setup (Shared Infrastructure)

No new project initialization required — feature lands in existing `crates/server` and `packages/guardian-operator-client`. Branch `007-dashboard-delta-details` already created by `/speckit.specify`.

- [x] T001 Confirm clean working tree on branch `007-dashboard-delta-details` and pull latest from `main` so the diff base is fresh.
- [x] T002 [P] Verify the Rust toolchain matches `rust-toolchain.toml` (1.93.0) — `rustup show active-toolchain` should print `1.93.0` so all downstream commands use the pinned version.
- [x] T003 [P] Verify the TypeScript workspace builds clean at HEAD — `npm install` + `npm run build --workspace @openzeppelin/guardian-operator-client` — so any new test failures are attributable to this feature, not pre-existing state. (Required fixing pre-existing duplicate re-export in `packages/guardian-operator-client/src/index.ts:19-25`.)

---

## Phase 2: Foundational — `delta_summary` module (Blocking Prerequisites)

**Purpose**: Stand up the shared decode + classify + project pipeline that every user story consumes. Lands with full unit coverage before any wire-facing change. Maps to Phase A in `plan.md`.

**⚠️ CRITICAL**: No user story work can begin until Phase 2 is complete.

- [x] T004 Create the module skeleton at `crates/server/src/delta_summary/mod.rs` with `pub mod decode;`, `pub mod category;`, `pub mod projection;`, and re-exports of the public surface from `data-model.md` (`DashboardDeltaCategory`, `DeltaActivitySummary`, `DecodedNote`, `DecodedAsset`, `VaultChange`, `StorageChange`, `DecodeWarning`, `NormalizedPayload`, `DetailIncludeFlags`, plus `classify` and `decode_full` function declarations). Add `mod delta_summary;` to `crates/server/src/lib.rs`.
- [x] T005 [P] Add the test fixtures at `crates/server/src/delta_summary/tests/fixtures.rs` per `research.md` Decision 10 — seven JSON constants: `MULTISIG_P2ID_WRAPPER`, `MULTISIG_P2ID_WRAPPER_BASE64`, `MULTISIG_ADD_SIGNER`, `MULTISIG_SWITCH_GUARDIAN`, `PUSH_DELTA_RAW_TX_SUMMARY`, `EVM_PLACEHOLDER`, `MALFORMED_BASE64`. Wire it via `#[cfg(test)] pub(crate) mod fixtures;` from `mod.rs`. Each fixture has a comment citing its expected `(category, kind)` result.
- [x] T006 Implement `NormalizedPayload::resolve(payload: &serde_json::Value) -> (NormalizedPayload, Vec<DecodeWarning>)` in `crates/server/src/delta_summary/decode.rs` with the three-way branch from `research.md` Decision 10: wrapper shape (`tx_summary` present, recursively handle JSON-inline vs `{data: base64}` sub-shapes), raw `TransactionSummary` shape (top-level `account_delta`), and opaque fallback. Inline tests assert each fixture from T005 resolves to the expected variant.
- [x] T007 Implement `classify(normalized: &NormalizedPayload) -> (DashboardDeltaCategory, Option<String>, DeltaActivitySummary)` in `crates/server/src/delta_summary/category.rs`. Use the `proposal_type → category` map from FR-002a for the `WithSummary { metadata: Some(_) }` branch; use the note-tag + account-delta inference from FR-002b for `WithSummary { metadata: None }`; return `("custom", None, empty_summary)` for `Opaque`. Inline tests cover every fixture and assert SC-002 (`category` never `None`).
- [x] T008 Implement `decode_full(normalized: &NormalizedPayload, include_scripts: bool) -> (Vec<DecodedNote>, Vec<DecodedNote>, Vec<VaultChange>, Vec<StorageChange>, Vec<DecodeWarning>)` in `crates/server/src/delta_summary/projection.rs`. Project `InputNotes` and `OutputNotes` per `data-model.md` (`DecodedNote` shape, `DecodedAsset` shape, optional `script` only when `include_scripts`). Project `AccountDelta` into `VaultChange` (signed-decimal for fungible, `added/removed` for non-fungible per Decision 4) and `StorageChange` (slot index + before/after). Inline tests cover at least one fungible + one non-fungible + one storage-slot case.
- [x] T009 Add the shared `build_entry_payload(delta: &DeltaObject) -> EntryPayload` helper at the bottom of `crates/server/src/delta_summary/mod.rs` (or `services/mod.rs` if that's more natural). It produces every field that both `DashboardDeltaEntry::from_delta` and `dashboard_global_deltas::entry_from` need (everything except `account_id`). This is the lockstep mechanism per `plan.md` Phase B.

**Checkpoint**: Run `cargo test -p guardian-server delta_summary` — every fixture passes, classifier never returns null category, decoder handles both payload shapes. Foundation ready.

---

## Phase 3: User Story 1 — Scannable activity feed (Priority: P1) 🎯 MVP

**Goal**: Both delta listing endpoints (`GET /dashboard/accounts/{account_id}/deltas`, `GET /dashboard/deltas`) return `category`, `kind`, and `summary` on every entry, with no removal/rename of existing fields and no perceptible ordering change.

**Independent Test**: Per `spec.md` Story 1 — seed an account with a mix of canonical-status deltas covering each known category, call both listing endpoints, verify every entry has a non-null `category`, every multisig entry has a non-null `kind`, every single-key push entry has `kind = null`, `summary.note_counts` is populated, and existing TS tests at `packages/guardian-operator-client/src/http.test.ts:570+` still pass.

- [x] T010 [US1] Extend `DashboardDeltaEntry` in `crates/server/src/services/dashboard_account_deltas.rs:40–60` with three new fields per `data-model.md`: `category: DashboardDeltaCategory` (always present), `kind: Option<String>` (serialize as `null`, not skip), and `summary: DeltaActivitySummary`. Keep the existing `proposal_type: Option<String>` field for backwards compat per Decision 7.
- [x] T011 [US1] Update `DashboardDeltaEntry::from_delta` (`:88–103`) to call `delta_summary::classify` on `delta.delta_payload`. On any error, the entry still ships with `category = "custom"`, `kind = None`, and an empty `DeltaActivitySummary` per FR-004 — the entry is never dropped.
- [x] T012 [US1] Extend `DashboardGlobalDeltaEntry` in `crates/server/src/services/dashboard_global_deltas.rs:49–62` to add the same three fields (`category`, `kind`, `summary`). This is a **flat duplicate struct**, not a `..DashboardDeltaEntry` spread — confirmed in `plan.md` Phase B notes.
- [x] T013 [US1] Update `entry_from` (`:107–118`) to call the shared `build_entry_payload` helper from T009 and copy its output into `DashboardGlobalDeltaEntry` along with `account_id`. This keeps `entry_from` and `DashboardDeltaEntry::from_delta` in lockstep.
- [x] T014 [P] [US1] Add inline `#[cfg(test)] mod tests` cases in `dashboard_account_deltas.rs` exercising the per-account listing for each category: p2id multisig, consume_notes multisig, add_signer multisig (asserts `kind = "add_signer"`, `category = "account_storage_change"`), switch_guardian multisig, single-key push (asserts `kind = None` but `category` derived), and the malformed-payload case (entry returned, `category = "custom"`).
- [x] T015 [P] [US1] Add the equivalent inline tests in `dashboard_global_deltas.rs` to prove `DashboardGlobalDeltaEntry` carries the same enrichment fields and the lockstep helper from T009 is reached. At least one cross-account assertion to catch any future drift.
- [x] T016 [US1] Extend the TS operator client types at `packages/guardian-operator-client/src/server-types.ts` to declare `DashboardDeltaCategory` (literal union of the 7 values), `DeltaActivitySummary`, the inner `asset`/`counterparty`/`note_counts` shapes, plus the extended `DashboardDeltaEntry` and `DashboardGlobalDeltaEntry`. Match `data-model.md` exactly.
- [x] T017 [US1] Update `parseDeltaEntry` in `packages/guardian-operator-client/src/http.ts:1338` to read `category` (required), `kind` (nullable), and `summary` (required, with nullable inner fields). `parseDeltaPage` at `:1254` and the global variant's parser need no change beyond the `parseDeltaEntry` update — verify by reading the call chain.
- [x] T018 [P] [US1] Extend the TS tests in `packages/guardian-operator-client/src/http.test.ts` — add new `it(...)` cases under the existing "GuardianOperatorHttpClient — per-account history" describe block at `:570` for: a p2id multisig entry parsed end-to-end (category + kind + summary asserted), a single-key push entry with `kind: null`, a malformed-summary tolerance case. Do not weaken any existing assertion at `:580+`.

**Checkpoint**: SC-001 / SC-002 / SC-006 / SC-007 satisfied end-to-end. Both listing endpoints return the new fields; existing dashboard smoke tests continue to pass. **This is the MVP — stop here if you want to ship just the activity feed.**

---

## Phase 4: User Story 2 — Delta detail view (Priority: P2)

**Goal**: `GET /dashboard/accounts/{account_id}/deltas/{nonce}` returns the decoded transaction effects (input/output notes, vault changes, storage changes) with optional debug fields behind `?include=scripts,raw`.

**Independent Test**: Per `spec.md` Story 2 — seed a canonical delta covering input notes, output notes, vault changes, and storage changes; fetch the detail endpoint; verify the response contains decoded projections for each section, default response excludes scripts and raw summary, `?include=scripts` adds the `script` field on every note, `?include=raw` adds top-level `raw_transaction_summary`.

- [x] T019 [US2] Created `crates/server/src/services/dashboard_account_delta_detail.rs` with `DashboardDeltaDetail` + `get_account_delta_detail(state, account_id, nonce, include)`. **Divergence from original task**: detail wire shape was later flattened (2026-05-25) — `category` + `proposal` at L1; `note_counts` / `asset` / `counterparty` deliberately NOT carried (derivable from per-section arrays).
- [x] T020 [US2] Re-exports in `services/mod.rs` (`get_account_delta_detail`, `DashboardDeltaDetail`, `DetailIncludeFlags`).
- [x] T021 [US2] `list_account_delta_detail_handler` added to `api/dashboard_feeds.rs`. **Divergence**: `?include=raw` reintroduced (2026-05-25) for debug — opt-in, base64 `raw_transaction_summary` in response. Note scripts remain dropped. Strict nonce parsing via `parse_canonical_nonce` per FR-009a.
- [x] T022 [US2] Route wired in `builder/handle.rs` immediately after the existing per-account deltas route. `route_layer` covers it automatically.
- [x] T023 [P] [US2] 7 inline tests in `dashboard_account_delta_detail.rs`: unknown-account / unknown-nonce DeltaNotFound, body-shape parity between the two, real-storage-error → DataUnavailable, canonical p2id projection happy path, `?include=raw` round-trip, undecodable-payload 200-with-warning.
- [x] T024 [P] [US2] 8 inline tests in `dashboard_feeds.rs` (`nonce_parse_tests`) covering accept-0, accept-typical, accept-u64-max; reject empty / hex / leading-zero / negative / non-decimal / out-of-range. Plus `?include=` parser tests.
- [x] T025 [US2] `getAccountDeltaDetail(accountId, nonce, options?)` in `http.ts` with `parseDeltaDetail` + sub-parsers for notes / vault / storage / warnings. `options.includeRaw` flows to `?include=raw` query param.
- [x] T026 [US2] TS types in `types.ts` (not `server-types.ts` — that file doesn't exist in this codebase): `DashboardDeltaDetail`, `DashboardDeltaDecodedNote`, `DashboardDeltaDecodedAsset`, `DashboardDeltaVaultChange` (tagged union), `DashboardDeltaStorageChange`, `DashboardDeltaDecodeWarning`, `DashboardDeltaNoteTag`, `DashboardDeltaDecodeSection`. Re-exported from `index.ts`.
- [x] T027 [P] [US2] TS tests for `getAccountDeltaDetail`: happy path, bigint nonce serialization, `decode_warnings` round-trip, unknown-note-tag rejection, `?include=raw` round-trip. **Divergence**: `?include=scripts` test dropped — that flag was removed from US2 scope.

**Checkpoint**: SC-005 satisfied. Detail endpoint serves structured data; debug fields opt-in; partial-decode tolerance works.

---

## Phase 5: User Story 3 — `{account_id, nonce}` key + uniform 404 (Priority: P2)

**Goal**: Reference key contract is stable across restarts, malformed segments fail with `400 InvalidInput` (distinct from 404), and the unknown-nonce / unknown-account 404 bodies are field-level identical so no info leak distinguishes them.

**Independent Test**: Per `spec.md` Story 3 — fetch a delta via the listing, use its `(account_id, nonce)` against the detail endpoint, assert the returned `nonce` equals the one from listing. Hit the detail endpoint with `-1`, `0xabc`, `0123`, and any non-decimal `{nonce}` — each returns `400 InvalidInput`. Hit it with a well-formed but unknown nonce on a known account and a well-formed nonce on an unknown account — both return 404 with byte-identical bodies (modulo timestamps if any).

- [x] T028 [US3] Strict `nonce` parser landed as `parse_canonical_nonce` in `api/dashboard_feeds.rs`; 8 unit tests in `nonce_parse_tests` cover accept-0, accept-typical, accept-u64-max, reject empty / hex / leading-zero / negative / non-decimal / out-of-range.
- [x] T029 [US3] `get_account_delta_detail` maps **both** unknown-account and unknown-nonce to `GuardianError::DeltaNotFound { account_id, nonce }` at the service boundary; code comment references SC-008.
- [x] T030 [P] [US3] Service-level test `unknown_account_and_unknown_nonce_share_the_same_error_body` in `dashboard_account_delta_detail.rs` diffs the two error bodies as Debug-formatted strings (equivalent to the field-level identity check the original task asked for; the underlying error variants are the same so the JSON serialization is identical).
- [x] T031 [P] [US3] Service-level listing→detail round-trip test `listing_to_detail_round_trip_preserves_nonce` in `dashboard_account_delta_detail.rs`. Mocks storage to serve the same delta to both `list_account_deltas` and `get_account_delta_detail`; asserts the nonce returned by the listing resolves the same delta via the detail endpoint, and that the dashboard fields (`prev_commitment`, `new_commitment`) agree across the two projections. Lives in the detail-service file rather than `dashboard_feeds.rs` because it doesn't need HTTP plumbing — purely a service-layer key-contract check.
- [x] T032 [P] [US3] TS listing→detail round-trip test `round-trips a listing entry nonce through getAccountDeltaDetail` in `http.test.ts`. Mocks two fetches (list + detail), calls `listAccountDeltas` then `getAccountDeltaDetail` with the returned nonce, asserts both nonces match and the detail URL carries the round-tripped value.

**Checkpoint**: SC-003 / SC-008 satisfied. URL parse rejection covers every malformed-key category from the spec edge cases. Round-trip stability proven.

---

## Phase 6: Polish & Cross-Cutting Concerns

- [ ] T033 [P] Criterion / `#[tokio::test]` timing bench for SC-004. **Deferred** by user decision — the perf trade-off is acknowledged in memory (`dashboard_delta_listing_perf.md`) and the architecture moved derivation to push time, so the per-listing decode cost has already been eliminated. Bench would just confirm the obvious. Re-open if telemetry suggests otherwise.
- [ ] T034 [P] Update `docs/dashboard.md`. **Skipped** — `docs/dashboard.md` does not exist in this codebase. CLAUDE.md references it as future work; not in scope for feature 007.
- [x] T035 [P] `packages/guardian-operator-client/README.md` updated for the spread-to-L1 wire shape (`entry.category` etc., no `entry.metadata` wrapper) and `getAccountDeltaDetail` with the `includeRaw` option. `includeScripts` not documented because it was dropped from US2 scope.
- [ ] T036 Run `quickstart.md` against a live Guardian server end-to-end. **User-driven** — best done by you in your local Postgres-backed environment per the smoke-web flow.
- [ ] T037 Final pass over `checklists/requirements.md`. Trivial scan; can land with the commit.
- [ ] T038 Full test suites green. **Verified at the last review pass**: server lib 514/0, TS operator-client 84/0, smoke-web build clean. Will re-run before commit.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies; T001 sequential, T002 + T003 parallel.
- **Foundational (Phase 2)**: Depends on Setup. T004 first; T005 + T006 can run in parallel after T004; T007 + T008 depend on T006; T009 can run after T006. **Blocks all user stories.**
- **User Story 1 (Phase 3, P1)**: Depends on Phase 2. T010 → T011 → (T014 in parallel); T012 → T013 → (T015 in parallel); TS side (T016 → T017 → T018) can proceed in parallel with the Rust side once T010/T012 land.
- **User Story 2 (Phase 4, P2)**: Depends on Phase 2 only; does not strictly require US1 but the two share `delta_summary`. T019 → T020 → T021 → T022; tests T023 + T024 in parallel after T021; TS side T025 + T026 + T027 in parallel.
- **User Story 3 (Phase 5, P2)**: Strongly depends on US2's handler + service existing (T021, T019). T028 lives inside the handler from T021; T029 lives inside the service from T019; T030–T032 are independent tests that can land in parallel once T028/T029 are in.
- **Polish (Phase 6)**: Depends on all desired user stories being complete. T033–T037 can run in parallel; T038 last.

### Within Each User Story

- Models / wire-shape structs land before the services that fill them.
- Services land before the handlers that call them.
- Handlers land before the router wiring.
- Tests sit colocated with the code they exercise (no separate `tests/` directory).
- TS-side changes can proceed in parallel with the Rust side once the wire shape is committed in the data-model.

### Parallel Opportunities

- T002 + T003 (toolchain + workspace verification, different stacks).
- T005 (fixtures) + T006 (decoder) — fixtures are a static data file; decoder can compile against them in parallel.
- T007 (classifier) + T008 (projector) — both consume the `NormalizedPayload` type from T006 but live in different files.
- US1 server-side (T010–T015) and US1 client-side (T016–T018) — once T010 fixes the wire shape, the two sides proceed independently.
- US2 inline tests (T023) + US2 HTTP tests (T024) + US2 TS tests (T025–T027) — different files.
- All US3 tests (T030–T032) — different files.
- All Polish tasks except T038 — different files.

---

## Parallel Example: Phase 2 Foundational

```bash
# After T004 (module scaffold) lands:
Task: "T005 — Add 7 payload fixtures in crates/server/src/delta_summary/tests/fixtures.rs"
Task: "T006 — Implement NormalizedPayload::resolve in crates/server/src/delta_summary/decode.rs"

# After T006 lands:
Task: "T007 — Implement classify(...) in crates/server/src/delta_summary/category.rs"
Task: "T008 — Implement decode_full(...) in crates/server/src/delta_summary/projection.rs"
Task: "T009 — Add build_entry_payload helper in crates/server/src/delta_summary/mod.rs"
```

## Parallel Example: User Story 1

```bash
# After T010 (DashboardDeltaEntry extended) + T012 (DashboardGlobalDeltaEntry extended) land:
Task: "T014 — Inline tests in crates/server/src/services/dashboard_account_deltas.rs"
Task: "T015 — Inline tests in crates/server/src/services/dashboard_global_deltas.rs"
Task: "T016 — Extend types in packages/guardian-operator-client/src/server-types.ts"

# After T016 lands:
Task: "T017 — Update parseDeltaEntry in packages/guardian-operator-client/src/http.ts"
Task: "T018 — Extend tests in packages/guardian-operator-client/src/http.test.ts"
```

---

## Implementation Strategy

### MVP (User Story 1 only)

1. Complete Phase 1: Setup (T001–T003).
2. Complete Phase 2: Foundational `delta_summary` (T004–T009). **Critical — blocks all stories.**
3. Complete Phase 3: User Story 1 (T010–T018).
4. **STOP and VALIDATE** — run the US1 section of `quickstart.md`; demo the enriched activity feed to a stakeholder.
5. Ship if approved; the dashboard now has a readable activity feed.

### Incremental Delivery

1. Setup + Foundational → Foundation ready.
2. Add User Story 1 → MVP shipped (enriched listing).
3. Add User Story 2 → Detail endpoint shipped.
4. Add User Story 3 → Reference-key contract hardened (uniform 404, strict parse).
5. Polish → Benchmark + docs + final test pass → Merge.

### Parallel Team Strategy

With multiple developers after Phase 2 completes:

- **Developer A**: Phase 3 (US1) — listing enrichment + TS client extension.
- **Developer B**: Phase 4 (US2) — detail service + handler + wiring + TS client.
- **Developer C**: Phase 5 (US3) — strict nonce parse + 404 body normalization + integration tests (waits on B's T021/T019 to merge before starting).

Phase 6 work can be split across the team.

---

## Notes

- [P] tasks = different files, no dependencies on incomplete tasks.
- [Story] label = US1 / US2 / US3 in the per-story phases; Setup, Foundational, and Polish phases have no story label.
- The full Validation Matrix in `plan.md` is the merge gate — every FR/SC maps to a task here.
- Stop at any checkpoint to validate the story independently before continuing.
- Do not introduce schema migrations, new env vars, or new error variants. Reuse `GuardianError::InvalidInput`, `DeltaNotFound`, `AccountNotFound`. (Confirmed against `crates/server/src/error.rs:131–152`.)
