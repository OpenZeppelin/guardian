# Research: Operator Authorization Foundation

**Feature Key**: `006-operator-authz` | **Date**: 2026-05-15

This document resolves the plan-phase decisions surfaced in the spec
and the design choices that fall out of mapping the spec onto the
existing codebase. Spec-level "NEEDS CLARIFICATION" markers: none.
What follows is design-level rationale for the eight decisions the
plan depends on.

## Decision 1: Extend the existing flat `ErrorResponse` envelope additively

**Decision**: Add `missing_permissions: Option<Vec<String>>` and
`retryable: Option<bool>` as new optional top-level fields on the
existing flat `ErrorResponse` in `crates/server/src/error.rs`. Do
NOT introduce a nested `error.{code,message,details,retryable}`
shape in this feature.

**Rationale**:

- The existing flat envelope (`{ success, code, error, retry_after_secs? }`)
  is parsed today by `parseErrorBody` at
  `packages/guardian-operator-client/src/http.ts:78-129` across many
  existing error codes. Switching to a nested shape would require
  re-routing every existing parser path and re-snapshotting every
  existing dashboard error test — work that has nothing to do with
  this feature and would block #181 unnecessarily.
- Additive extension keeps the existing 200-byte error responses for
  every other code byte-for-byte unchanged. Only
  `GUARDIAN_INSUFFICIENT_OPERATOR_PERMISSION` responses carry the new
  fields.
- The broader nested-envelope migration is [#179](https://github.com/OpenZeppelin/guardian/issues/179)'s
  job. Spec §Assumption 10 explicitly keeps this code forward-
  compatible — when #179 lands its envelope, this code rides it
  without a re-spec.

**Alternatives considered**:

- *Ship nested envelope now*: rejected. Bigger blast radius than
  #181 deserves; couples two features that should land
  independently.
- *Inline `details` only on the new code*: rejected. Adds a parser
  branch in `parseErrorBody` for one code; the flat-extension form
  keeps the parser uniform.

**Implications for FR-016**: pin the additive form. The plan's
"Error code wire-through" workstream depends on this.

---

## Decision 2: Enforce `admin_actions` append-only at the Postgres trigger layer

**Decision**: Add a Postgres trigger
`admin_actions_no_update` BEFORE `UPDATE OR DELETE` that raises an
exception. The Rust `Auditor` trait separately exposes no update or
delete method, so the application-level surface is also clean — but
the **enforcement** layer is the trigger.

**Rationale**:

- "Append-only" must be more than convention (FR-026). A trait that
  exposes only `record()` is convention; a trigger is enforcement.
- A future migration that adds a `WHERE id = ?` cleanup path (for
  retention work, hypothetical operator-error correction, etc.)
  would be code-only — it would not silently bypass append-only
  because the trigger fires. Retention work would have to explicitly
  drop the trigger as part of its own migration, which is the
  audit-trail-of-the-audit-trail we want.
- Trigger overhead is negligible at this volume (tens of rows/day).

**Alternatives considered**:

- *Application-only*: rejected. Same reason as above. Doesn't
  survive a code refactor.
- *Postgres rule (`ON UPDATE DO INSTEAD NOTHING`)*: rejected.
  Postgres rules silently no-op rather than error; we want an
  explicit failure so a buggy code path surfaces in tests rather
  than silently swallowing modifications.
- *Row-level security*: rejected. Overkill, adds a role-management
  axis Guardian does not have today.

**Implications for FR-026**: pin trigger-based enforcement. Note in
the migration that retention work in a follow-up will need to drop
this trigger explicitly.

---

## Decision 3: Parse the heterogeneous JSON with `#[serde(untagged)]`

**Decision**: Use a serde `untagged` enum:

```rust
#[derive(Deserialize)]
#[serde(untagged)]
enum AllowlistEntryWire {
    LegacyHex(String),
    Structured { public_key: String, permissions: Vec<String> },
}
```

**Rationale**:

- `untagged` matches the JSON shape exactly: a string deserializes
  as `LegacyHex`; an object with the two named fields deserializes
  as `Structured`. No discriminator field, no `kind: "string"` noise
  in the wire format.
- The existing legacy `Vec<String>` shape is the
  `LegacyHex`-only case of the new array, so no migration step is
  needed for existing JSON files / secrets.
- An object missing `permissions` will fail both arms and surface a
  clear `untagged` error — matches Edge Case 4 ("object entry
  missing `permissions` is rejected as malformed").

**Alternatives considered**:

- *Two-pass parse (try `Vec<String>` first, fall back to
  `Vec<Value>`)*: rejected. Double-parsing for no semantic gain;
  brittle error messages.
- *Externally tagged*: rejected. Forces a `"kind"` field on every
  entry, breaks the legacy shape.
- *Custom `Deserialize` impl*: rejected. `untagged` already does
  exactly this; custom code is needless ceremony.

**Implications**: simple, ~10-line `AllowlistEntryWire` type plus a
mapping pass into the in-memory `OperatorAllowlist` that validates
hex, applies legacy-grant default, deduplicates permissions, and
checks `public_key` uniqueness across entries.

---

## Decision 4: Log-fallback selector is `audit.admin_action`; fields mirror the row 1:1

**Decision**: The `LogAuditor` emits one `tracing::warn!` per event
at `target = "audit.admin_action"` with structured fields
`occurred_at`, `operator_identity`, `action_kind`, `target_account_id`,
`payload`, `outcome`, `error_code`. The level is `warn` (not `info`)
so the events are visible by default in production log
collection — audit data is not chatty enough to spam, and the level
makes it easy to scrape.

**Rationale**:

- One greppable selector across all deployments means a security
  reviewer's runbook works identically on Postgres-backed and
  filesystem-backed deployments — they `grep audit.admin_action` and
  see the same field set.
- Mirroring the Postgres column names 1:1 means structured log
  consumers (Loki, CloudWatch, etc.) can ingest both surfaces into
  the same schema once a deployment plumbs the log pipeline.
- `warn` level matches existing security-relevant log entries in
  Guardian (see `crates/server/src/dashboard/` log lines). The
  startup advisory that "audit is not persisted" is also `warn`.

**Alternatives considered**:

- *`info` level*: rejected. Some deployments filter info-level
  noise; audit must remain visible.
- *Custom log macro that bypasses `tracing`*: rejected. Loses
  structured field support.
- *Different field names from the Postgres column names*: rejected.
  Forces a mapping layer for consumers.

**Implications for FR-021**: pin the selector name and the field
names.

---

## Decision 5: Probe endpoint gated by Cargo feature `authz-test-probe`, default off

**Decision**: Add `authz-test-probe = []` to `crates/server/Cargo.toml`
features, default off in release builds. The probe route is
registered inside `#[cfg(feature = "authz-test-probe")]` blocks in
`builder/handle.rs`. CI runs the dashboard authz integration tests
with `--features authz-test-probe`.

**Rationale**:

- Cargo features are the existing precedent for build-time gating in
  this repo (e.g. the `evm` server feature). Operators can audit
  whether a build includes the probe by listing the features used in
  the build artifact.
- `#[cfg(test)]`-only would block the operator-smoke-web harness from
  exercising the probe against a release-shaped server. Cargo
  feature lets CI build with the probe in while production deploys
  build without it.
- The probe's `accounts:pause` requirement means even if it
  accidentally shipped, only pause-capable operators could hit it,
  and the only side effect is one `admin_actions` row.

**Alternatives considered**:

- *Environment variable toggle*: rejected. A toggle that flips on at
  runtime is harder to audit and harder to reason about in security
  review.
- *`#[cfg(test)]` only*: rejected (above).
- *Always on*: rejected. The probe has no production purpose; it
  shouldn't ship.

**Implications**: CI and smoke tests must opt in via
`--features authz-test-probe`. Document this in `quickstart.md`.

---

## Decision 6: HTTP-only operator surface, no gRPC parity (documented divergence)

**Decision**: Operator surface remains HTTP-only. No gRPC mapping for
the new middleware or the new error code.

**Rationale**:

- `crates/server/proto/guardian.proto:6-42` defines a single
  `service Guardian` carrying only account/state RPCs (`Configure`,
  `PushDelta`, `GetState`, etc.). There is no operator gRPC surface
  to be parity-with.
- Adding operator endpoints to gRPC is a separate scope decision
  with its own client work; the architecture document defers it.
- This is the same divergence already documented and accepted in
  [`005-operator-dashboard-metrics`](../005-operator-dashboard-metrics/plan.md)
  per Constitution §II.

**Alternatives considered**:

- *Mirror to gRPC*: rejected. Out of scope; no operator gRPC
  consumer exists.

**Implications for Constitution Check**: documented divergence,
inherited from 005.

---

## Decision 7: Extend `AuthenticatedOperator` in place rather than wrap it

**Decision**: Add `effective_permissions: Arc<BTreeSet<Permission>>`
to the existing `AuthenticatedOperator` struct
(`crates/server/src/dashboard/types.rs:6-10`) rather than wrapping it
in a new `AuthorizedOperator` type.

**Rationale**:

- Existing handlers pull `Extension<AuthenticatedOperator>` from
  request extensions. Wrapping would require updating every handler
  that consumes this extension — bigger blast radius for no
  semantic gain.
- `Arc<BTreeSet<Permission>>` makes the field cheap to clone into
  the request extension and into audit events.
- The new field is read-only after session-middleware sets it; no
  handler mutates the principal. No risk of inconsistent state.

**Alternatives considered**:

- *Wrapper type*: rejected. Touches every handler; adds an
  indirection level for no callers' benefit.
- *Two separate `Extension`s (`AuthenticatedOperator` +
  `OperatorPermissions`)*: rejected. Splitting the principal across
  two extensions invites bugs where one is consumed without the
  other. They should be a single principal.

**Implications**: handler signatures unchanged.

---

## Decision 8: `action_kind` registry lives in `crates/server/src/audit/kinds.rs`

**Decision**: A new module `crates/server/src/audit/kinds.rs`
exports `pub const` strings for every recognized `action_kind`.
This feature reserves `AUTH_DENIED` and `PROBE_ACCESS`. Future
features add their own consts in the same module.

**Rationale**:

- One place to look for the full audit vocabulary makes incident
  response trivial — `git log -p audit/kinds.rs` is the entire
  history of "what kinds of actions has Guardian ever audited".
- Consts (not enums) keep consumer features' PRs small — a new
  `pub const ACCOUNTS_PAUSE: &str = "accounts.pause";` line is
  sufficient. An enum would require touching every match site.
- Pinning the location avoids each consumer feature inventing its
  own constant module.

**Alternatives considered**:

- *Enum*: rejected. Match sites add coupling that consts don't.
- *Per-feature module*: rejected. Audit vocabulary fragmentation.
- *Inline string literals*: rejected. Typo risk; not auditable.

**Implications for FR-024**: pin the path. Make it a one-liner for
#181 / #182 PRs to land their own `action_kind` consts.

---

## Deferred / non-decisions

- **Retention policy for `admin_actions`**: deferred (spec §Out of
  Scope). Triggers must be dropped explicitly by future retention
  work (Decision 2).
- **`admin_actions` read endpoint**: deferred. `psql` + log
  collection are the v1 query paths.
- **Cross-replica session sharing**: out of scope; sessions remain
  in-process.
- **Disjunctive ("any of") permission requirements**: FR-011
  explicitly forbids in v1.
- **Hot-reload file watcher**: out of scope; the existing
  reload-on-authenticate path is sufficient.
- **JSONL filesystem audit sink**: superseded by Decision 4 (log
  fallback covers the filesystem-backend case without a new sink).
