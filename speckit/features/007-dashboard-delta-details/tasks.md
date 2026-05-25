---

description: "Task list for feature 007 — dashboard delta activity feed and detail view"
---

# Tasks: Dashboard delta activity feed and detail view

**Input**: Design documents from `/Users/zeljkomarkovic/Documents/Projects/guardian/speckit/features/007-dashboard-delta-details/`
**Prerequisites**: plan.md ✅, spec.md ✅, research.md ✅, data-model.md ✅, contracts/ ✅, quickstart.md ✅

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

- [ ] T019 [US2] Create `crates/server/src/services/dashboard_account_delta_detail.rs` with `pub struct DashboardDeltaDetail { ... }` per `data-model.md` and `pub async fn get_account_delta_detail(state: &AppState, account_id: &str, nonce: u64, include: DetailIncludeFlags) -> Result<DashboardDeltaDetail>`. The service looks up the delta by `(account_id, nonce)` using the existing storage adapter, normalizes the payload via `NormalizedPayload::resolve`, then calls `classify` + `decode_full` from the foundational module.
- [ ] T020 [US2] Add `pub mod dashboard_account_delta_detail;` and the re-export to `crates/server/src/services/mod.rs` next to the existing `dashboard_account_deltas` / `dashboard_global_deltas` lines.
- [ ] T021 [US2] Add `list_account_delta_detail_handler` to `crates/server/src/api/dashboard_feeds.rs` next to `list_account_deltas_handler` at `:55`. Extracts `Path<(String, String)>` for `(account_id, nonce_str)`, parses `?include=` (comma-list of `scripts` / `raw`, anything else ignored), and translates `nonce_str` parse failure to `GuardianError::InvalidInput` per FR-009a. Calls `get_account_delta_detail` and serializes the `Result<Json<DashboardDeltaDetail>>`.
- [ ] T022 [US2] Wire the new route in `crates/server/src/builder/handle.rs` immediately after the existing `.route("/accounts/{account_id}/deltas", get(list_account_deltas_handler))` at `:100–103`: `.route("/accounts/{account_id}/deltas/{nonce}", get(list_account_delta_detail_handler))`. The existing `route_layer(from_fn_with_state(dashboard_read_authz, enforce_authz))` at `:111` covers this route automatically since it's registered before the layer.
- [ ] T023 [P] [US2] Add inline `#[cfg(test)] mod tests` in `dashboard_account_delta_detail.rs` covering the 5 acceptance scenarios from `spec.md` Story 2: full p2id detail shape; cross-account 404 (wrong account in path); Guardian-switch detail with empty `input_notes`; default response excludes `script` field; partial-decode delta returns 200 with `decode_warnings[]` populated and other sections still filled.
- [ ] T024 [P] [US2] Add inline HTTP tests in `dashboard_feeds.rs` (next to the existing per-account-deltas tests at `:289+`) for: 200 happy path, `?include=scripts` round-trip, `?include=raw` round-trip, `?include=scripts,raw` combined, unknown query value ignored.
- [ ] T025 [US2] Add `getAccountDeltaDetail(accountId: string, nonce: number | bigint, options?: { includeScripts?: boolean; includeRaw?: boolean }): Promise<DashboardDeltaDetail>` to `packages/guardian-operator-client/src/http.ts`. Builds the path `dashboard/accounts/{encoded}/deltas/{nonce}` and the `?include=` query from the options. Add a `parseDeltaDetail` parser that mirrors the server contract.
- [ ] T026 [US2] Add `DashboardDeltaDetail`, `DecodedNote`, `DecodedAsset`, `VaultChange`, `StorageChange`, `DecodeWarning` types in `packages/guardian-operator-client/src/server-types.ts` matching the server shapes from `data-model.md`.
- [ ] T027 [P] [US2] Add TS tests in `packages/guardian-operator-client/src/http.test.ts` for `getAccountDeltaDetail`: happy path, `?include=scripts` flag flows through, `?include=raw` flag flows through, nonce passed through correctly as a path segment.

**Checkpoint**: SC-005 satisfied. Detail endpoint serves structured data; debug fields opt-in; partial-decode tolerance works.

---

## Phase 5: User Story 3 — `{account_id, nonce}` key + uniform 404 (Priority: P2)

**Goal**: Reference key contract is stable across restarts, malformed segments fail with `400 InvalidInput` (distinct from 404), and the unknown-nonce / unknown-account 404 bodies are field-level identical so no info leak distinguishes them.

**Independent Test**: Per `spec.md` Story 3 — fetch a delta via the listing, use its `(account_id, nonce)` against the detail endpoint, assert the returned `nonce` equals the one from listing. Hit the detail endpoint with `-1`, `0xabc`, `0123`, and any non-decimal `{nonce}` — each returns `400 InvalidInput`. Hit it with a well-formed but unknown nonce on a known account and a well-formed nonce on an unknown account — both return 404 with byte-identical bodies (modulo timestamps if any).

- [ ] T028 [US3] In the handler from T021, implement the strict `nonce` parser per FR-009a: reject negative numbers, hex (`0x`-prefixed or otherwise), leading zeros except `"0"` itself, underscores, and any non-`u64`-decimal input. Each rejection returns `GuardianError::InvalidInput` with a stable message. Inline tests in `dashboard_feeds.rs` cover each rejection case explicitly.
- [ ] T029 [US3] In `get_account_delta_detail` (T019), normalize the 404 response so `GuardianError::DeltaNotFound` and `GuardianError::AccountNotFound` produce field-level identical JSON bodies per SC-008. The cleanest path is to map both to a single `GuardianError::DeltaNotFound { account_id, nonce }`-shaped body at the service boundary; document the chosen approach in a brief code comment that references SC-008.
- [ ] T030 [P] [US3] Add an inline integration test in `dashboard_feeds.rs` that diffs the two 404 response bodies as `serde_json::Value`: one request hits a known account with an unknown nonce, the other hits an unknown account; assert the two `Value`s are equal modulo any per-request fields (e.g., trace ids).
- [ ] T031 [P] [US3] Add an inline test in `dashboard_feeds.rs` exercising the listing→detail round-trip: list one page of deltas, take the first entry's `nonce`, fetch the detail with that nonce, assert detail's `nonce` equals listing's.
- [ ] T032 [P] [US3] Add a TS integration test in `packages/guardian-operator-client/src/http.test.ts` exercising the listing→detail round-trip through the client surface (`listAccountDeltas` then `getAccountDeltaDetail` with the returned nonce).

**Checkpoint**: SC-003 / SC-008 satisfied. URL parse rejection covers every malformed-key category from the spec edge cases. Round-trip stability proven.

---

## Phase 6: Polish & Cross-Cutting Concerns

- [ ] T033 [P] Add a Criterion bench (or `#[tokio::test]` timing harness if Criterion overkill) in `crates/server/src/services/dashboard_account_deltas.rs` per `research.md` Decision 8 — seed ~500 mixed-shape canonical deltas in the filesystem backend, run baseline (pre-feature shape) vs. enriched, capture p95. Acceptance: enriched p95 within the baseline envelope. Numbers go in the PR description for the SC-004 sign-off.
- [ ] T034 [P] Update `docs/dashboard.md` with the enriched listing shape (`category`/`kind`/`summary`), the new detail endpoint URL + response shape, the `?include=` query parameter, and the v1 authorization scope note (no per-account ACL).
- [ ] T035 [P] Update `packages/guardian-operator-client/README.md` with the new `getAccountDeltaDetail` method signature, an example, and a note about the `includeScripts` / `includeRaw` options.
- [ ] T036 Run `quickstart.md` against a local Guardian server end-to-end (US1 / US2 / US3 sections). Capture any drift between the doc and reality; fix the doc.
- [ ] T037 Final pass over `checklists/requirements.md` — verify every box is justified by a landed test or doc. Update the checklist file if any item is still open.
- [ ] T038 Run the full server test suite (`cargo test -p guardian-server`) and the full TS operator-client test suite (`npm test --workspace @openzeppelin/guardian-operator-client`) on the feature branch. Both green is the merge bar.

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
