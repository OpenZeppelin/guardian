# Implementation Plan: Standards-Based Database TLS Certificate Verification

**Branch**: `001-db-tls-verification` | **Date**: 2026-06-04 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `speckit/features/001-db-tls-verification/spec.md`

## Summary

Replace the async Postgres path's accept-any-certificate verifier with proper,
standards-based TLS verification driven entirely by the standard libpq
connection-string parameters (`sslmode`, `sslrootcert`) already understood by
the synchronous migration path. The connection string becomes the single source
of truth so both TLS stacks (sync libpq migrations, async rustls pools) enforce
identical behavior. The fix is provider-neutral (AWS RDS, GCP, Azure, local TLS)
via an operator-supplied CA bundle file; no vendor CA is embedded.

Resolved design decisions (from `/speckit.plan` clarifications):
- **Trust anchor** = explicit CA bundle file via `sslrootcert=<path>` only.
  `sslrootcert=system` is rejected for now (needs libpq ≥16; runtime image is
  PG15). No Docker image change in this feature.
- **`allow`/`prefer`** = rejected at startup (no silent plaintext fallback).
- **`require` + `sslrootcert`** = promoted to `verify-ca` (follow libpq), applied
  identically on both stacks.

## Technical Context

**Language/Version**: Rust (edition 2024), `crates/server` (`guardian-server`)
**Primary Dependencies**: `diesel 2.2` (libpq, sync migrations), `diesel-async 0.5` + `tokio-postgres 0.7.17` + `tokio-postgres-rustls 0.12` + `rustls 0.23.37` (async pools); `url 2.5` (already a dep) for connection-string parsing; **new**: `rustls-pemfile` (PEM CA loading). `rustls-native-certs 0.8` / `webpki-roots 1.0` are already transitively in the lockfile (not needed unless system store is implemented — out of scope here).
**Storage**: PostgreSQL (feature-gated `postgres`); filesystem backend unaffected.
**Testing**: `cargo test -p guardian-server --features postgres` (unit + parser/verifier tests); manual smoke for live managed providers (AGENTS.md §6); local TLS Postgres for an automated/scripted verification leg.
**Target Platform**: Linux server; runtime image `debian:bookworm-slim` + `libpq5` (PG15) for the server binary.
**Project Type**: Single Rust workspace; server crate + Terraform infra + docs.
**Performance Goals**: TLS config built once per pool (Arc-shared into the connection setup closure), not per-connection; no measurable hot-path impact.
**Constraints**: Must not break local plaintext / encrypt-only deployments (FR-006); credentials must stay redacted in errors (FR-005a); two TLS stacks must not diverge (FR-007).
**Scale/Scope**: One crate module (`storage/postgres.rs`) + one Cargo dep + Terraform `data.tf` + docs. No HTTP/gRPC surface change, no schema/migration change.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **I. Bottom-Up Change Propagation** — PASS. Change is confined to the server's
  storage connection layer. No client/SDK/wire contract changes; nothing
  propagates upstream. The only callers of the TLS path are the three Postgres
  consumers + migrations within `crates/server`.
- **II. Transport & Cross-Language Parity** — PASS / N/A. No HTTP/gRPC surface or
  Rust↔TS behavior is touched.
- **III. Append-Only Integrity & Explicit Lifecycles** — PASS, and reinforced.
  The change *removes* an implicit insecure path and *forbids* silent plaintext
  fallback (rejecting `allow`/`prefer`). No state/delta/proposal semantics
  touched. The `require`+rootcert promotion is explicit and documented.
- **IV. Explicit Auth & Stable Boundary Errors** — PASS. This hardens transport
  authentication to the database. New startup/connection errors are explicit and
  actionable; credentials remain redacted (FR-005a). No status enum / payload
  changes.
- **V. Evidence-Driven Delivery** — PASS. Independently testable user stories
  (P1/P2/P3) with a targeted validation plan (unit tests for parsing + verifier
  selection; local-TLS integration leg; documented manual smoke for managed
  providers). Docs updates are in scope (FR-008, FR-010).

**System Invariant of note**: "Storage backends preserve the same externally
observable semantics unless a documented backend-specific limitation is
accepted." This change keeps Postgres externally equivalent; the only new
backend-specific limitation is that `sslrootcert=system` is unsupported until a
libpq upgrade — explicitly documented (FR-003a). **No violations; Complexity
Tracking not required.**

## Project Structure

### Documentation (this feature)

```text
speckit/features/001-db-tls-verification/
├── plan.md              # This file
├── spec.md              # Feature spec
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output (config/verification model — no DB schema)
├── quickstart.md        # Phase 1 output (operator + dev verification walkthrough)
├── contracts/
│   └── tls-verification-contract.md   # sslmode/sslrootcert → behavior matrix (the "contract")
└── checklists/requirements.md
```

### Source Code (repository root)

```text
crates/server/
├── Cargo.toml                        # + rustls-pemfile dependency under `postgres` feature
└── src/
    ├── storage/
    │   └── postgres.rs               # PRIMARY: replace NoCertificateVerification; add
    │                                 #   sslmode/sslrootcert parser (pub(crate)), verifier
    │                                 #   selection, URL sanitization for tokio-postgres,
    │                                 #   Arc<ClientConfig> capture in the custom_setup closure
    └── builder/
        └── storage.rs                # ALSO: call the shared preflight/validate BEFORE
                                      #   run_migrations (the enforcement point for FR-007/G6);
                                      #   inject sslmode=disable when absent (parity normalization)

infra/
└── data.tf / ecs.tf                  # DATABASE_URL: sslmode=require → verify-full&sslrootcert=<path>
                                      #   Mount a COMBINED CA bundle (Amazon RDS roots + Amazon Trust
                                      #   Services roots) at a fixed container path at DEPLOY time
                                      #   (FR-009a/b) — prod routes through the RDS Proxy endpoint
                                      #   (ACM cert → ATS roots), direct instance uses RDS roots, so
                                      #   both roots are required. The published image stays CA-free.
                                      #   (no Dockerfile change — image is provider-neutral)

docs/
├── CONFIGURATION.md                  # document sslmode levels + sslrootcert trust anchor
├── TROUBLESHOOTING.md                # verification-failure → cause mapping (FR-010)
├── LOCAL_DEV.md                      # local TLS test instructions (verify-ca vs verify-full)
└── SERVER_AWS_DEPLOY.md / runbooks   # RDS CA delivery + rotation procedure (FR-009a)
```

**Structure Decision**: Single-crate change centered on
`crates/server/src/storage/postgres.rs`. The verification behavior is a
connection-config contract, not a network API, so `contracts/` holds the
`sslmode`/`sslrootcert` → behavior matrix rather than an OpenAPI/GraphQL schema.

## Design Overview

**Enforcement point — a shared preflight BEFORE migrations (critical).** FR-007/G6
parity is NOT guaranteed by hoping libpq and rustls independently reach the same
verdict — libpq is more permissive (it accepts `allow`/`prefer`, defaults absent
`sslmode` to `prefer`, and looks up `~/.postgresql/root.crt` for verify modes with
no `sslrootcert`). The production startup runs migrations FIRST
(`builder/storage.rs:117-119`: `run_migrations` → `PostgresService::new` →
`PostgresMetadataStore::new`), so a rejection that only lives in the async pool
path would fire *after* libpq already connected (possibly over plaintext). The
plan therefore adds a **Guardian-level preflight** that parses and validates
`(sslmode, sslrootcert)` ONCE and rejects `allow`/`prefer`/`system`/unknown/
verify-without-`sslrootcert` **before `run_migrations` is called**. Both stacks
are then governed by the same gate. This preflight lives in `builder/storage.rs`
(in the `postgres` build path, ahead of the migration call), using a parser
exposed from `storage/postgres.rs`.

The async path is then taught to mirror libpq. For a TLS-capable connection:

0. **Preflight (shared gate)**: parse `(sslmode, sslrootcert)`; reject the
   unsupported/insecure combinations and normalize the absent-`sslmode` default
   (see step 1a) — before migrations and before pool creation.
1. **Parse** `sslmode` and `sslrootcert` from `DATABASE_URL` using the `url` crate.
1a. **Normalize absent `sslmode`**: libpq defaults absent `sslmode` to `prefer`
   (TLS-with-plaintext-fallback), but the async path would treat absent as
   plaintext — a divergence on a TLS-capable server. The preflight normalizes
   absent `sslmode` to `disable` on BOTH stacks (it injects `sslmode=disable` into
   the URL handed to the sync migration path), so the two stacks agree and local
   plaintext dev keeps working (FR-006). Operators who want TLS MUST set an
   explicit `sslmode`. This normalization is documented (least-surprise + fail-safe).
2. **Resolve effective level** (the normalization table — see contracts):
   - `disable` → no TLS (plain manager, no custom_setup).
   - `require` + no `sslrootcert` → encrypt-only (no-verify verifier; the ONLY
     remaining non-verifying verifier, permitted because encrypt-only is not a
     verifying mode per FR-004).
   - `require` + `sslrootcert` → `verify-ca` (libpq promotion, FR-001c).
   - `verify-ca` → chain-only verifier (custom verifier delegating chain
     validation to `WebPkiServerVerifier`, tolerating hostname mismatch).
   - `verify-full` → rustls default `WebPkiServerVerifier` (chain + hostname;
     free hostname checking via tokio-postgres-rustls passing the host as
     `ServerName`).
   - `allow`/`prefer`/unknown/`sslrootcert=system` → **fail fast** at pool
     creation with an actionable, credential-redacted error.
3. **Build `rustls::ClientConfig` once**, wrap in `Arc`, capture (clone) into the
   `ManagerConfig::custom_setup` closure (avoids rebuilding per connection).
4. **Three explicit URL values** (resolves prior "untouched URL" ambiguity):
   - `raw_url` — exactly what the operator supplied (never connected with directly
     once parsed).
   - `normalized_sync_url` — for libpq/migrations: absent `sslmode` → `disable`
     injected; for verifying modes an **explicit `sslrootcert=<path>` is always
     present** so libpq never falls back to `~/.postgresql/root.crt` (FR-007a).
     libpq honors `verify-ca`/`verify-full` + `sslrootcert` natively (PG15-OK for
     file paths).
   - `sanitized_async_url` — for tokio-postgres (which errors on unknown params):
     strip `sslrootcert` and any non-Disable/Prefer/Require `sslmode`, set
     `sslmode=require` to force TLS; our rustls verifier enforces the real level.
   Both stacks derive from the same parsed `(sslmode, sslrootcert)` → parity.
5. **Neutralize implicit libpq trust sources**: the runtime image carries no
   `PGSSL*` env or `~/.postgresql/root.crt` (verified). Combined with the explicit
   `sslrootcert` in `normalized_sync_url`, the migration stack cannot diverge via
   libpq defaults. Parity (FR-007) is scoped to `DATABASE_URL` inputs in this
   controlled image.

See [research.md](./research.md) for the version-accurate API findings behind
each step, [data-model.md](./data-model.md) for the parsed-config model and
state transitions, and [contracts/tls-verification-contract.md](./contracts/tls-verification-contract.md)
for the authoritative behavior matrix.

## Phase 2 Notes (for `/speckit.tasks`)

Suggested task ordering (not the task list itself):
1. **Spike first (highest-risk):** confirm the exact `rustls 0.23.37` API for a
   chain-only / hostname-skipping verifier (research D4) by building the
   `verify-ca` verifier behind a unit test. Finding a dead-end here early avoids
   rework. Also wire `rustls-pemfile = "2"` and write the CA loader so it
   **collects and propagates** PEM parse errors (no `filter_map(Result::ok)` that
   would silently drop malformed certs — feeds FR-005a).
2. Introduce the parsed-config type + `sslmode`/`sslrootcert` parser with unit
   tests covering every behavior-matrix row (no behavior change yet).
3. **Add the shared preflight in `builder/storage.rs` BEFORE `run_migrations`**
   (the FR-007/G6 enforcement point) + absent-`sslmode`→`disable` normalization.
4. Implement verifier selection (encrypt-only / verify-ca / verify-full) + CA
   bundle loading + fail-fast, credential-redacted errors; remove
   `NoCertificateVerification` from the verifying paths.
5. Wire URL sanitization for the async stack + Arc<ClientConfig> capture; confirm
   the sync migration path passes the full (normalized) URL.
6. Update Terraform `data.tf` + RDS CA delivery/rotation (FR-009/FR-009a).
7. Docs: CONFIGURATION, TROUBLESHOOTING, LOCAL_DEV, AWS deploy/runbook.
8. Validation:
   - Unit: every behavior-matrix row; parsing edge cases (duplicate `sslmode`/
     `sslrootcert`, empty `sslrootcert=`, percent-encoded paths, non-URL
     keyword/value DSN, unsupported scheme, multi-host) all fail deterministically;
     CA loader rejects a partially-malformed bundle; redaction test (FR-005a).
   - **P3 explicit (FR-006):** omitted `sslmode` → plaintext; `disable` →
     plaintext; `require` (no rootcert) → TLS no-verify; `require` → refuses a
     non-TLS server — each asserted on BOTH the migration and pool paths.
   - **Hostname (FR-002a):** cross-stack tests for DNS-SAN match, IP-SAN match,
     SAN mismatch (refused), CN-only cert (refused).
   - **Preflight ordering:** `allow`/`prefer`/`system`/unknown/verify-without-
     rootcert rejected BEFORE any connection (no insecure migration precedes it).
   - Local-TLS integration leg (automated/scripted) for verify-ca + verify-full.
   - Manual smoke (AGENTS.md §6): AWS RDS via the **RDS Proxy endpoint** (proxy
     enabled AND disabled) + one other managed provider, using the combined
     bundle; `verify-full` success + wrong-CA refusal.

## Complexity Tracking

> No constitution violations — section intentionally empty.
