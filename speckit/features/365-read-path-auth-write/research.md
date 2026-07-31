# Phase 0 Research: Approach Selection for the Replay-Protection Write (FR-005)

**Feature**: `365-read-path-auth-write` | **Date**: 2026-07-31
**Purpose**: The documented, security-weighed comparison of candidate designs required by FR-005 before implementation. Candidates include the three options sketched in issue #365 plus four alternatives identified during codebase analysis.

## Current mechanism (ground truth from code)

- Every account-scoped request flows through `resolve_account` (`crates/server/src/services/mod.rs:101-191`): metadata `get()` → skew check (`MAX_TIMESTAMP_SKEW_MS = 300_000`, i.e. 5 minutes, `crates/server/src/metadata/auth/credentials.rs:6`) → signature verify → `update_last_auth_timestamp_cas` → on CAS failure, `AuthenticationFailed("Replay attack detected…")` (`services/mod.rs:177-186`).
- The CAS is a `MetadataStore` trait method (`crates/server/src/metadata/mod.rs:179-184`) with two implementations:
  - **Postgres** (`crates/server/src/metadata/postgres.rs:267-300`): conditional `UPDATE account_metadata SET last_auth_timestamp, updated_at` — rewrites the full MVCC tuple including the `auth` JSONB (~900 bytes/key for Falcon multisig).
  - **Filesystem** (`crates/server/src/metadata/filesystem.rs:184-207`): mutates the in-memory cache, then `self.persist(&cache)` — **rewrites the entire metadata cache file on every authenticated read**. The same disease in a different form.
- `last_auth_timestamp` lives on `AccountMetadata` (`metadata/mod.rs:24`) and is threaded through every construction site, but **never reaches any client-visible surface** — no API response, no dashboard field exposes it. It exists solely for the CAS.

### Two latent defects the current placement causes (beyond cost)

1. **Stale-clobber race**: `PostgresMetadataStore::set()` writes `last_auth_timestamp` from the struct (`postgres.rs:162,178`). Any read-modify-write flow that loads metadata, then calls `set()`, can overwrite a *newer* replay timestamp recorded by a concurrent authenticated request with the *stale* value it loaded — silently regressing replay protection for that account. Two live sites carry the field this way: `configure_account.rs:150` (loads `existing` at flow start, then holds it across the initial-state network submission before `set()` — the widest such window) and `evm/service.rs:133` (same preserve-the-field pattern).
2. **Dashboard ordering churn**: the CAS bumps `updated_at`, which is the dashboard pagination sort key (`postgres.rs:236-239` orders by `updated_at DESC`, with a dedicated pagination index). Read traffic therefore continuously reshuffles dashboard account ordering and destabilises cursor walks.

Both defects disappear structurally under the recommended option, independent of the performance win.

## Evaluation criteria (from FR-005)

1. **Security posture**: durability + atomicity of replay state; behavior on crash/failover; cross-replica consistency; migration fail-mode.
2. **Performance recovered**: fraction of the measured +71% upper bound (`results/nocas-128-t4-20260730T092323Z`).
3. **Operational complexity**: new runtime dependencies, migration risk, self-hosted-deployment footprint.
4. **Deployment constraints**: multi-replica correctness, rolling-upgrade behavior.

## Candidate designs

### A. Narrow-table split (in-database) — **CHOSEN**

Move the hot column into `account_auth_state(account_id PK, last_auth_timestamp)`, a ~40-byte row, CAS unchanged in form. Backfill from `account_metadata` in the same migration transaction; drop the old column.

- **Security**: identical to today — durable, atomic, shared across replicas via the primary. The exactly-once guarantee is byte-for-byte the same SQL predicate. Migration is transactional: replay state carries over atomically (FR-006). **No security decision required.**
- **Performance**: MVCC tuple rewrite drops from >1 KB to ~40 bytes; WAL volume collapses proportionally; HOT updates become achievable (no secondary indexes on the table, `fillfactor` tuned below 100 to keep same-page space); the state `SELECT` recovers the measured MVCC-churn tax (0.213 → 0.178 ms in the no-CAS experiment). One durable commit per read remains — this recovers a large fraction, not all, of the +71%. Whether it clears SC-001 (≥25%) is exactly the experiment FR-007 mandates.
- **Operational**: one embedded Diesel migration, run automatically at startup (`storage/postgres.rs:87`). No new dependency. Filesystem backend gets the analogous split (auth state persisted in its own small file instead of rewriting the whole cache).
- **Deployment**: mixed-version fleet **fails closed** — after the column drop, an old-binary replica's metadata `SELECT` (which names the column, `schema.rs:86`) errors, so it serves 500s rather than authenticating against stale state. No replay hole; old replicas are dead weight until replaced. Documented as an operator note.
- **Bonus**: fixes the stale-clobber race and the dashboard `updated_at` churn (defects 1 and 2 above) by construction, and satisfies FR-008 for free since the CAS no longer touches `account_metadata` at all.

### B. External auth-state store (ElastiCache/Redis, DynamoDB) — rejected for this feature

- **Security**: replay state in Redis is volatile by default — a crash/failover can lose up to the full 5-minute skew window of state, reopening a bounded replay window. That is an explicit weakening requiring security sign-off (FR-001). DynamoDB avoids volatility but couples the OSS server to AWS.
- **Performance**: the only option that approaches the full +71% (removes the write from Postgres entirely).
- **Operational**: new runtime dependency for every deployment mode — compose guides, local dev, AWS Terraform, the diagnostic stack. Heavy for a self-hostable OSS server whose published image already mandates Postgres.
- **Verdict**: premature, and not selectable inside this feature at all (FR-001 restricts selection to guarantee-preserving candidates). If Option A's measured result misses SC-001/SC-002, this returns to the user as the escalation path — via a spec amendment or successor feature, not a perf tweak.

### C. Coarsened persistence granularity (persist only when advancing > N ms) — rejected

Weakens the guarantee by construction: replays within N ms succeed. FR-001 forbids adopting this implicitly, and there is no need to spend the security budget before the cheap, guarantee-preserving option has been measured. The `/state/lookup` precedent (documented bounded window, `services/lookup_account.rs:16-19`) exists because that path *has no account to key state against* — not applicable here where per-account state is available.

### D. UNLOGGED narrow table — rejected as default; noted as a measured-escalation variant

`UNLOGGED` on the Option-A table eliminates WAL entirely, but Postgres **truncates unlogged tables on crash recovery** — a crash fails *open* (all replay state gone, every captured request in the skew window becomes replayable once). Worse than Redis's failure mode because it is silent. Only worth revisiting alongside Option B — through the same spec-amendment path FR-001 requires — and only if measurement shows WAL (not tuple churn) dominates the residual cost.

### E. Relaxed commit durability (`SET LOCAL synchronous_commit = off` for the CAS) — rejected

Bounds state loss to the WAL-flush interval on crash (a small replay window — still an FR-001 decision). The evidence says the bottleneck is Postgres **CPU** (pegged at ~165% of cap in every leg), not commit latency, so the expected gain is small relative to its security cost. Could compose with A later if measurement contradicts this.

### F. Protocol-level redesign (client nonces, server challenges, session tokens) — out of scope

Changes the wire contract and both SDKs; violates FR-004 and the spec's out-of-scope list ("the authentication protocol itself is untouched").

### G. Async write-behind / replica-local caching of the CAS — rejected

Any window between "request accepted" and "state durable+shared" is a replay window, and replica-local state is defeated by replaying to the other replica — precisely the threat model the CAS exists for (issue #104 / commit `ae78e3b`).

## Decision

**Option A: narrow-table split**, in both storage backends, with the FR-007 benchmark A/B as the acceptance gate.

**Rationale**: it is the only candidate that is simultaneously (a) guarantee-preserving with no security decision needed, (b) dependency-free, (c) fail-closed under every deployment edge case examined (crash, failover, rolling deploy, migration), and (d) the direct test of the issue's row-width hypothesis. It also structurally eliminates two latent correctness defects. Options B/D/E all buy additional throughput by spending security posture; the spec (FR-001, Assumptions) says that trade may only be made deliberately, after A's residual cost is measured — not preemptively.

**Escalation path (pre-agreed)**: if the A/B re-run misses SC-001 (≥25% throughput) or SC-002 (auth cost floor), the measured residual goes back to the user with Options B and D framed as what they are — guarantee-weakening trades that FR-001 places outside this feature, reachable only through a spec amendment or a successor feature. The same report covers the SC-006 stretch marker (≥40% headroom) even when the feature passes, so the user can judge whether the residual justifies opening that discussion.

## Design sub-decisions for Option A

| # | Decision | Rationale | Alternatives considered |
|---|---|---|---|
| A1 | Single-statement upsert-CAS: `INSERT … ON CONFLICT (account_id) DO UPDATE SET last_auth_timestamp = EXCLUDED.last_auth_timestamp WHERE account_auth_state.last_auth_timestamp < EXCLUDED.last_auth_timestamp`; affected-rows 0 ⇒ replay | One round trip; handles first-auth and steady-state uniformly; no registration-flow coupling; account existence already gated by the metadata `get()` earlier in `resolve_account` | Backfilling a row for every account at migration + plain `UPDATE` at runtime — rejected: couples registration to a second insert and makes a missing row ambiguous |
| A2 | No `updated_at` (or any other) column on `account_auth_state` | The table has exactly one job; extra columns re-import churn and blur FR-008 | Mirroring `updated_at` — rejected as churn with no consumer |
| A3 | `fillfactor` below 100 on the table | Leaves same-page free space so updates stay HOT (no index maintenance, page-local pruning); rows are ~40 bytes so the space cost is trivial | Default fillfactor 100 — packed pages defeat HOT |
| A4 | FK `REFERENCES account_metadata(account_id) ON DELETE CASCADE` | Hygiene: no orphan auth state; FK adds cost only at row insert (once per account), not per CAS update | No FK — rejected: silent orphans on any future account-deletion path |
| A5 | Drop `last_auth_timestamp` from `account_metadata` and from the `AccountMetadata` struct (`metadata/mod.rs:24`) in the same change | Leaving the field is a silent-drift trap (two sources of truth) and keeps the stale-clobber race alive; AGENTS.md §3 forbids compat shims | Keep column temporarily for rollback — rejected: creates the exact dual-write ambiguity the feature removes; rollback is restore-from-migration instead |
| A6 | Trait signature becomes `update_last_auth_timestamp_cas(&self, account_id: &str, new_timestamp: i64) -> Result<bool, String>` (drop `now`) | The `now` parameter existed only to bump `updated_at`, which FR-008 abolishes | Keep unused param — rejected: dead parameter contradicts repo style |
| A7 | Filesystem backend: auth-state map persisted to its own small file (`auth_state.json`), written atomically and created immediately at first startup (seeded once from legacy `last_auth_timestamp` values in existing metadata files) | Preserves the durable, exactly-once semantics within the backend's single-process deployment model (constitution: backends preserve externally observable semantics; the filesystem backend is single-process by design across all operations — pre-existing limitation, inherited unchanged) and FR-006 across upgrade; stops rewriting the whole metadata cache per read | In-memory only — rejected: silently downgrades filesystem durability (FR-001); per-account files — rejected: more moving parts for a dev/test backend with a process-wide write lock already |
| A8 | Migration performs create + backfill + column drop in one transaction | Diesel embedded migrations run transactionally at startup; replay state is never absent nor duplicated at any observable point (FR-006) | Two-phase migration across releases — rejected: fail-closed mixed-version behavior (see Deployment note) makes the extra release unnecessary |

## Resolved unknowns (Technical Context had no NEEDS CLARIFICATION remaining)

- **Does anything read `last_auth_timestamp` besides the CAS?** No. All other references are struct-construction sites (tests, mocks, registration paths constructing `None`) and the `evm/service.rs:133` preserve-workaround, which the split deletes.
- **How do migrations reach production?** Embedded and run at startup (`crates/server/src/storage/postgres.rs:34,87`); no operator action needed.
- **Mixed-version behavior after column drop?** Old binaries fail closed (their `SELECT` names the dropped column). Verified against `schema.rs:86` + Diesel's explicit column lists.
- **Benchmark reproduction path?** `benchmarks/diagnostic-stack/` with `profiles/diag-read-128.toml`, exactly as issue #365 documents. The harness is **not yet in this repo's history** — it lands via a separate pending PR, making it an external dependency of FR-007 verification (recorded in the spec's Assumptions). If its baseline result directories don't ship with the merge, baseline legs are regenerated on unmodified `main` on the same machine — all headline numbers are same-machine A/B by rule.
