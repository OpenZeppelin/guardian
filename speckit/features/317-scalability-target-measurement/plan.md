# Implementation Plan: Guardian Scalability Target — Measurement Definition

**Branch**: `317-scalability-target-measurement` | **Date**: 2026-07-28 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/317-scalability-target-measurement/spec.md`

## Summary

Make the issue #317 scalability target measurable, then measure it. The work is
in three movements: define each target dimension so two runs cannot disagree
about what was measured; extend the existing distributed benchmark harness so it
can offer the target load shape and account population; publish a baseline that
states the numeric distance to target per dimension.

The technical core is a change in load model. The harness today is closed-loop:
every seeded user is a task that issues the next operation the instant the last
one returns (`runner.rs:142`), so "N users" means "N requests permanently in
flight" and offered load is an output of server latency rather than an input.
The target is stated in users, and the resolved definition (FR-003) makes a user
a client at a declared rate — so the harness needs a paced, open-loop mode where
offered load is an input. Everything else follows: paced users mostly sleep, so
20,000 of them cost far less than 20,000 saturating ones, and a run can finally
distinguish "the server is slow" from "the generator is saturated".

Second core change: the account population must decouple from the user count.
The profile schema pins `accounts_per_user == 1` (`config.rs:95`), so 20,000
readers can only ever imply 20,000 accounts. Reaching 100,000 needs a separate
population concept — a real-path subset that load actually touches plus a bulk
subset that exists to create scale (FR-016a).

Everything else is reporting fidelity the April round lacked: auth-window
failures separated from generic auth failures, a real unclassified bucket,
build identity captured at both ends of the run, per-criterion verdicts, and
saturation attribution.

## Technical Context

**Language/Version**: Rust (workspace toolchain), Bash for orchestration
**Primary Dependencies**: `guardian-client` (gRPC), `tokio`, `clap`, `serde`,
`toml`, `chrono`, AWS CLI, ECS/Fargate, CloudWatch Logs, Session Manager plugin
**Storage**: Postgres behind RDS Proxy on the deployment under test; benchmark
artifacts as JSON/Markdown under `benchmarks/prod-server/reports/<run-id>/`
**Testing**: `cargo test` for harness unit logic (shard math, classification,
percentile, verdicts); live runs are the integration surface
**Target Platform**: macOS/Linux for orchestration; ARM64 Fargate for workers and
for the server under test; Docker Compose for the diagnostic tier (Guardian
replicas behind Caddy, Postgres, Prometheus, Grafana)
**Project Type**: Benchmark and measurement tooling around an existing service —
no product surface, no client SDK change, no wire-contract change
**Performance Goals**: The harness must offer 2,000 `get_state`/s sustained
(20,000 readers × 0.1/s) and 100 concurrent transacting users without the
generator becoming the bottleneck
**Constraints**: gRPC only in the prod harness (`Transport::Grpc` is the sole
variant); the measured deployment is live production, so runs must clean up after
themselves and must not require server reconfiguration to execute
**Scale/Scope**: 100,000-account population, ~20,100 of them real-path; measured
windows ≥ 5 minutes; distributed across ECS worker tasks

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Assessment | Verdict |
|---|---|---|
| **I. Bottom-Up Change Propagation** | This feature changes no server contract and no client behaviour. It observes an existing surface from a benchmark crate that sits outside the server→client→SDK→examples chain. Nothing upstream consumes it. | **PASS** |
| **II. Transport and Cross-Language Parity** | No transport semantics change. The prod harness measures gRPC only, which is a pre-existing harness limitation, not a divergence this feature introduces. HTTP/gRPC comparison stays with the local harness, which already covers both. No TypeScript surface is touched. | **PASS** |
| **III. Append-Only Integrity and Explicit Lifecycles** | The harness reads canonicalization lifecycle state (`canonical_at` / `discarded_at`) and never writes it. One design point needs care: bulk account provisioning (FR-016a) writes rows through a privileged path rather than through `configure`. That path MUST produce records indistinguishable from real-path records, and FR-016b's sample-equivalence check is the enforcement. It creates accounts; it does not rewrite state or shortcut a lifecycle transition. | **PASS**, with FR-016b as the guard |
| **IV. Explicit Authentication and Stable Boundary Errors** | This feature depends on distinguishing auth-window expiry from other auth failures. On the wire both are `GuardianError::AuthenticationFailed` with only the message differing (`services/mod.rs:133`). The plan classifies client-side on the message substring rather than changing the server's error surface — deliberately, since a new error code would be a contract change requiring propagation under Principle I. Recorded as a known fragility in research.md, not silently absorbed. | **PASS** |
| **V. Evidence-Driven Delivery** | The whole feature is evidence production. Five independently testable stories, each with acceptance scenarios; harness logic changes get `cargo test` coverage; run outputs are the validation artifact. | **PASS** |

No violations. Complexity Tracking table omitted.

**Post-design re-check (after Phase 1 artifacts)**: Design added two things worth
re-testing against the constitution. Bulk account provisioning (research §3)
writes through the privileged ECS-exec channel rather than `configure` — it
creates accounts and never rewrites state or shortcuts a lifecycle transition, and
FR-016b's equivalence check gates any verdict that depends on it, so Principle III
still holds. The diagnostic tier (research §13–16) adds an environment, not a
surface: same harness binary, same profile schema, no server or client code
change, and it may optionally exercise HTTP without altering HTTP/gRPC semantics,
so Principle II holds. Both new report and profile fields are additive
(contracts), so the April reports stay parseable. **Re-check: PASS.**

## Project Structure

### Documentation (this feature)

```text
speckit/features/317-scalability-target-measurement/
├── spec.md              # Feature specification
├── plan.md              # This file
├── research.md          # Phase 0: harness findings and design decisions
├── data-model.md        # Phase 1: entities and their relationships
├── quickstart.md        # Phase 1: how to run a measurement
├── contracts/
│   ├── profile-schema.md      # Benchmark profile TOML contract
│   └── run-report-schema.md   # Run report JSON contract
└── tasks.md             # Phase 2 output (/speckit.tasks — NOT created here)
```

### Source Code (repository root)

```text
benchmarks/prod-server/          # Authoritative harness (FR-017a)
├── src/
│   ├── config.rs                # + load pacing, + population, − accounts_per_user pin
│   ├── runner.rs                # + paced loop, + in-flight gauge, − inline canon polling
│   ├── workload.rs              # + pacing schedule alongside operation_for_index
│   ├── error_classification.rs  # + auth_window, + unclassified
│   ├── report.rs                # + verdicts, + build identity, + saturation, + offered/achieved
│   ├── model.rs                 # + BuildIdentity, + Verdict, + ClockSample
│   ├── seed.rs                  # + bulk population provisioning, + resumability
│   ├── preflight.rs             # + status capture, + clock check, + population check
│   ├── status.rs                # NEW: /status capture over plain HTTP
│   ├── population.rs            # NEW: population manifest + real-path/bulk split
│   └── verdict.rs               # NEW: target versions and per-criterion evaluation
├── profiles/
│   ├── target-read-20k.toml         # NEW
│   ├── target-write-100.toml        # NEW
│   ├── target-combined.toml         # NEW
│   └── replay-*.toml                # NEW: April profiles pinned for replay
├── targets/
│   └── scalability-317.toml     # NEW: versioned target definition (FR-001)
└── reports/<run-id>/            # Run artifacts

benchmarks/diagnostic-stack/     # NEW: diagnostic tier (FR-017a)
├── docker-compose.yml           # Composed from the two existing guide stacks
├── prometheus/prometheus.yml    # Scrape config incl. all replicas
├── grafana/                     # Provisioned dashboards
└── variants/                    # One-variable-at-a-time overrides (FR-017d)
    ├── pool-16.env  pool-32.env  pool-64.env
    ├── fast-promotion-{on,off}.env
    └── replicas-{1,2,4}.env

crates/server/bench/             # Local fast loop, non-authoritative
└── README.md                    # Fix: stale "Benchmark Runtime Code Switch" section

scripts/run-prod-benchmark-ecs.sh  # Worker fleet sizing for paced runs

docs/
└── benchmarks/                  # FR-026 operator documentation
```

**Structure Decision**: Three tiers, one harness. `benchmarks/prod-server` stays
the authoritative harness and gains the new modules; `crates/server/bench` stays
the fast loop; `benchmarks/diagnostic-stack` is a new **environment**, not a new
harness — the same `worker-run` binary points at `localhost` instead of the
public endpoint. The distributed harness already provides worker sharding,
artifact aggregation, canonicalization sampling, and benchmark-owned cleanup, so
no third load generator is warranted. New concerns that do not fit an existing
module (status capture, population management, verdict evaluation) become their
own modules rather than accreting onto `runner.rs`.

The diagnostic stack is assembled from two committed, working compose files —
`docs/guides/horizontal-scaling/docker-compose.yml` (N replicas behind a Caddy
proxy, shared Postgres, shared ACK identity) and
`docs/guides/observability/docker-compose.yml` (Prometheus + Grafana) — rather
than authored fresh.

## Phase Ordering

| Phase | Delivers | Tier | Gated on |
|---|---|---|---|
| **0** | Target definition (`targets/scalability-317.toml`), measurement definition doc, classification fixes, `/status` capture, verdict scaffolding | — | — |
| **1** | Diagnostic stack assembled; server-side metrics available; one-variable comparisons (pool size, fast promotion, replica count) | Diagnostic | Phase 0, Docker only |
| **2** | US1 replay: April profiles pinned and re-run, comparison against published April numbers | Authoritative | Phase 0, AWS access |
| **3** | Paced load model, in-flight gauge, saturation attribution, canonicalization off the critical path | Both | Phase 0 |
| **4** | Population split, bulk provisioning, resumability | Both | Phase 3 |
| **4b** | N-account funded fixture (generalise PR #348's 2-account fixture); canonicalization criterion measured on the real-account workload | Real-account | PR #348 landing, faucet funding |
| **5** | Target-load profiles, combined run, published baseline with gap table | Authoritative | Phases 2–4b |

Phases 1 and 2 are independent and can run in parallel — Phase 1 needs only
Docker, Phase 2 needs only AWS. Phase 1 can start immediately with no external
dependency, and it is where bottleneck evidence has to come from: production
exposes no server-side metrics at all (`GUARDIAN_METRICS_ENABLED` defaults to
`false` and `infra/*.tf` never sets it), so the authoritative tier can produce a
verdict but never a diagnosis.

Phase 2 needs no deploy: the deployed build is already current — `6e7c626`,
tree-identical to `ce4c342` on main.

## Complexity Tracking

No constitution violations. Section intentionally empty.
