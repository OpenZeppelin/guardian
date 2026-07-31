# Implementation Plan: Reduce Per-Read Cost of Replay-Protection Auth Writes

**Branch**: `365-read-path-auth-write` | **Date**: 2026-07-31 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/365-read-path-auth-write/spec.md`, GitHub issue #365, approach comparison in [research.md](./research.md)

## Summary

Every signature-authenticated request performs a replay-protection compare-and-set against `account_metadata.last_auth_timestamp` (`crates/server/src/services/mod.rs:162-186`). Because that column shares an MVCC tuple with the multi-kilobyte `auth` JSONB, each eight-byte update rewrites the whole row — measured at 33.5% of read-path database time, with +71% throughput available when removed. The chosen approach (research.md, Option A) moves the timestamp into a dedicated narrow table, `account_auth_state(account_id, last_auth_timestamp)`, keeping the CAS predicate — and therefore the security guarantee — byte-for-byte identical while collapsing the per-update tuple from >1 KB to ~40 bytes. The filesystem backend gets the analogous split (its CAS currently rewrites the entire metadata cache file per read). No wire-contract change; no client change; the FR-007 benchmark A/B is the acceptance gate. Guarantee-weakening options (external store, UNLOGGED, coarsening) are documented in the comparison but are not selectable within this feature (FR-001): if the ≥25% throughput target is missed, the measured residual returns to the user, and those options are reachable only via a spec amendment or successor feature.

## Technical Context

**Language/Version**: Rust (workspace toolchain, edition 2024); server crate `crates/server`
**Primary Dependencies**: diesel / diesel-async (Postgres, embedded migrations), tokio, existing `MetadataStore` trait machinery — no new dependencies
**Storage**: PostgreSQL (feature-gated backend; the published image mandates it) and filesystem backend (dev/test default) — both change, preserving identical externally observable semantics
**Testing**: `cargo test -p guardian-server` (filesystem default), Postgres-gated integration tests, two-replica diagnostic stack (`benchmarks/diagnostic-stack/`) for replay verification and the benchmark A/B
**Target Platform**: Linux server (Docker/ECS), macOS local dev
**Project Type**: Multi-crate Rust workspace with TS packages; this feature is server-only (`crates/server`)
**Performance Goals**: SC-001 ≥25% throughput over the 2,314/s baseline on the 128-reader leg, p95 no worse (SC-006 headroom floor is the same gate restated); SC-002 auth share of DB time 43.5% → ≤25% **and** auth DB time per accepted read −40% (baseline ~0.16ms); SC-007 mixed-profile A/B (40/40/20 `get_state`/`get_delta_since`/`get_delta_proposals`)
**Constraints**: Replay-protection guarantee unchanged — weaker variants require a spec amendment, not a design decision (FR-001); wire contract frozen (FR-004); migration must carry replay state over atomically (FR-006); fail-closed under mixed-version fleets
**External Dependency**: FR-007/SC-001–003/SC-007 verification requires the `benchmarks/diagnostic-stack/` harness, which lands via a separate pending PR (not part of this feature's scope); baselines are regenerated on unmodified `main` if the recorded result dirs don't ship with it
**Scale/Scope**: 100k-account deployments, 2,000 paced reads/s (#317 target); change surface ≈ 1 migration, 2 backend impls, 1 trait signature, ~20 mechanical struct-site updates

## Constitution Check

*GATE: evaluated against Guardian Constitution v1.1.0 — PASS (pre-research and re-checked post-design).*

| Principle | Assessment |
|---|---|
| I. Bottom-Up Change Propagation | Change is confined to `crates/server`. Upstream consumers (Rust/TS clients, SDKs, examples) are proven unaffected by the frozen wire contract ([contracts/auth-replay-contract.md](./contracts/auth-replay-contract.md)) and re-proven by SDK smoke flows (SC-005). |
| II. Transport & Cross-Language Parity | HTTP and gRPC share `resolve_account`; both surfaces change identically by construction. No client-visible behavior changes, so Rust/TS parity is untouched. |
| III. Append-Only Integrity & Explicit Lifecycles | No state/delta/proposal lifecycle is touched. Replay state is auth metadata, not lineage. No fallback paths introduced; the CAS failure path keeps its explicit error. |
| IV. Explicit Auth & Stable Boundary Errors | High-risk area, handled per the principle: the CAS predicate and the `AuthenticationFailed` replay error are preserved exactly; tests are updated in the changed layer (metadata backends) plus upstream consumers (service-level integration tests through HTTP/gRPC). |
| V. Evidence-Driven Delivery | Acceptance is measured: benchmark A/B vs committed baselines (FR-007), replay tests per backend (FR-009), two-replica verification (SC-003), SDK smokes (SC-005). |
| Invariant: "Per-account authentication remains explicit and replay-protected" | Preserved — the guarantee's predicate, atomicity, durability, and error surface are unchanged; only the storage placement moves. |
| Invariant: "Storage backends preserve the same externally observable semantics" | Both backends keep a durable, atomic, exactly-once CAS within their supported deployment models — cross-process for Postgres, single-process for the filesystem backend (a pre-existing, documented limitation of that backend across all its operations, inherited unchanged). The filesystem backend additionally *gains* correctness (stops rewriting unrelated metadata per read). |

No violations → Complexity Tracking is empty.

## Project Structure

### Documentation (this feature)

```text
speckit/features/365-read-path-auth-write/
├── spec.md              # Feature specification
├── plan.md              # This file
├── research.md          # Phase 0: FR-005 approach comparison (decision: narrow-table split)
├── data-model.md        # Phase 1: schema, struct, trait, and file-format changes
├── quickstart.md        # Phase 1: verification & benchmark A/B runbook
├── contracts/
│   └── auth-replay-contract.md   # Phase 1: frozen wire surface + internal contract delta
└── tasks.md             # Phase 2 (/speckit.tasks — not created here)
```

### Source Code (repository root)

```text
crates/server/
├── migrations/
│   └── 2026-07-31-000001_account_auth_state/
│       ├── up.sql       # CREATE account_auth_state + backfill + DROP old column
│       └── down.sql
└── src/
    ├── schema.rs                    # + account_auth_state table; − last_auth_timestamp column
    ├── services/mod.rs              # resolve_account: CAS call site (signature only)
    ├── metadata/
    │   ├── mod.rs                   # AccountMetadata struct (− field); trait signature change
    │   ├── postgres.rs              # upsert-CAS against account_auth_state; set() stops writing the field
    │   └── filesystem.rs            # split auth-state map + own persistence file + legacy seed
    ├── evm/service.rs               # remove the preserve-last_auth_timestamp workaround
    └── testing/                     # mocks, fixtures, integration tests (mechanical + new replay tests)

benchmarks/diagnostic-stack/         # A/B verification harness (exists; used, not modified)
```

**Structure Decision**: Server-only change inside the existing `crates/server` layout — one new migration directory, edits to the two `MetadataStore` backends and the shared struct/trait, plus mechanical updates at struct-construction sites (`api/grpc.rs`, `api/http.rs`, `api/dashboard*.rs`, `jobs/canonicalization/processor.rs`, `services/*`, `testing/*`). No new crates, packages, or top-level directories.

## Phase Outline

### Phase 0 (complete → [research.md](./research.md))

Seven candidate approaches evaluated against FR-005's four criteria (security posture, performance recovered, operational complexity, deployment constraints): the issue's three options plus UNLOGGED tables, relaxed commit durability, protocol redesign, and async write-behind. **Decision: narrow-table split** — the only guarantee-preserving, dependency-free, fail-closed candidate; it also structurally fixes two latent defects (the `set()` stale-clobber race at `postgres.rs:162,178` and dashboard `updated_at` ordering churn). Escalation path pre-agreed: if the benchmark misses SC-001/SC-002, the residual returns to the user; external-store/UNLOGGED options are outside this feature by FR-001 and reachable only through a spec amendment or successor feature.

### Phase 1 (complete → design artifacts)

- [data-model.md](./data-model.md) — `account_auth_state` DDL (fillfactor, FK, no extra columns), upsert-CAS statement, `AccountMetadata` struct change, trait signature, filesystem auth-state file format and legacy-seed rule, migration/backfill semantics, mixed-version fail-closed analysis.
- [contracts/auth-replay-contract.md](./contracts/auth-replay-contract.md) — the frozen external surface (endpoints, error semantics for replay/skew rejection) and the internal contract delta (trait, schema).
- [quickstart.md](./quickstart.md) — verification runbook: unit/integration tests, two-replica replay checks, benchmark A/B against the committed baselines, SDK smoke flows.

### Phase 2 (next: `/speckit.tasks`)

Expected shape: migration + schema task, Postgres backend task, filesystem backend task, trait/struct/call-site sweep, test tasks per FR-009, then the verification runbook (quickstart) as the acceptance pass. The benchmark A/B is the last gate before the feature is called done, per Constitution V.

## Complexity Tracking

*No constitution violations — table intentionally empty.*
