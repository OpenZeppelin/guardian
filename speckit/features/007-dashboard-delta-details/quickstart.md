# Quickstart — Validating feature 007 locally

End-to-end loop for verifying the enriched listing endpoints (and the scaffolded detail endpoint) against a real server. Assumes you've already followed [`docs/QUICKSTART.md`](../../../docs/QUICKSTART.md) once and have the server runnable.

> **2026-05-25 revision** — this feature now adds a new `metadata JSONB` column on `deltas` (migration `2026-05-25-000001_delta_metadata`). Postgres users must `diesel migration run` before exercising the new wire shape. Filesystem-backed dev servers need no migration (serde handles the new field transparently). The listing wire shape changed: the top-level `category` / `kind` / `summary` / `proposal_type` fields from earlier drafts are replaced by a single `metadata` object with derived fields + optional `proposal` block. See `data-model.md` for the authoritative shape.

## Prerequisites

- Rust toolchain (workspace pin from `rust-toolchain.toml`, currently 1.93.0).
- Node + npm (for the TS operator client tests).
- Filesystem-backed dev server is sufficient for local validation. **Postgres users**: run `diesel migration run` against your local DB so the new `metadata` column exists before restarting the server. See [`docs/LOCAL_DEV.md`](../../../docs/LOCAL_DEV.md).
- A running Guardian server with at least one account that has executed mixed-shape transactions. The `examples/demo` CLI plus the `smoke-test-rust-multisig-sdk` and `smoke-test-ts-multisig-sdk` skills can seed this — see those skills for the canonical procedures.
- Operator dashboard auth: the dashboard uses a cookie session (`guardian_operator_session`, see `crates/server/src/dashboard/config.rs:7`), not Bearer tokens. Obtain a session by running `auth/challenge` + `auth/verify` per the operator-client README (or via the `smoke-test-operator-dashboard` skill) and export the cookie value as `OPERATOR_SESSION` for the curl examples below. From a logged-in browser, copy the cookie value with DevTools → Application → Cookies.

## Seed: one of each category

To exercise category inference, the local store needs at least one canonical delta for each category. Smallest set:

| Category | Easiest source |
|---|---|
| `asset_transfer` | `examples/demo` → multisig p2id transfer to a recipient |
| `note_consumption` | `examples/demo` → consume a previously-created note |
| `account_storage_change` | `examples/demo` → multisig `add_signer` (also exercises the `proposal.proposal_type` field) |
| `guardian_switch` | `examples/demo` → multisig `switch_guardian` |
| `note_creation` (no-input variant) | direct `push_delta` of a single-key Miden transaction that only creates a note |
| `custom` | direct `push_delta` of a transaction whose payload doesn't match any inference rule |


## Story 1 — Activity feed (enriched listing)

```bash
# Per-account
curl -s -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas?limit=10" | jq

# Global
curl -s -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/deltas?limit=10" | jq
```

Check per-entry:
- Each entry has either a `metadata` object or no `metadata` key at all. "Metadata unavailable" (no `metadata` key) is the expected state only for EVM deltas, pre-feature-007 historical rows, and undecodable payloads.
- When present, `metadata.category` is one of the seven enum values and is never `null`.
- `metadata.proposal.proposal_type` is set on multisig-sourced deltas; for single-key push deltas the entire `metadata.proposal` block is absent.
- `metadata.note_counts.input` and `.output` are present and correct.
- `metadata.asset` is populated for `p2id` (from proposal metadata for multisig, from the first output note for single-key push).
- All pre-existing fields (`nonce`, `status`, `prev_commitment`, `new_commitment`, `proposal_type`) are present and unchanged.

## Story 2 — Detail view

Take a `nonce` from a listing entry above. Then:

```bash
# Default
curl -s -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas/${NONCE}" | jq

# With note scripts (debug)
curl -s -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas/${NONCE}?include=scripts" | jq

# With raw transaction summary (debug)
curl -s -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas/${NONCE}?include=raw" | jq
```

Check:
- `input_notes`, `output_notes`, `vault_changes`, `storage_changes` are all present (possibly empty arrays, never null, never absent).
- `script` field on decoded notes appears only when `?include=scripts` is set.
- `raw_transaction_summary` appears only when `?include=raw` is set.
- `decode_warnings[]` appears only when something failed to decode.

## Story 3 — Key stability

```bash
# Listing → detail round-trip
NONCE=$(curl -s -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas?limit=1" \
  | jq -r '.items[0].nonce')

curl -s -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas/${NONCE}" \
  | jq '.nonce'

# Restart server, repeat — same nonce must resolve.
```

## Negative paths

```bash
# Malformed nonce → 400 (FR-018)
curl -i -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas/-1"

curl -i -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas/0xabc"

# Unknown nonce → 404, body indistinguishable from cross-account / unauthorized (SC-008)
curl -i -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/${ACCOUNT_ID}/deltas/99999999"

# Wrong account (delta exists elsewhere) → same 404 body
curl -i -b "guardian_operator_session=$OPERATOR_SESSION" \
  "http://localhost:3000/dashboard/accounts/0xunrelated/deltas/${NONCE}"
```

The three `404` bodies must be **byte-identical** at the field-level (status code may differ only if any of these surface as a different existing error class — but per spec, they don't).

## Test entry points

All server tests live in inline `#[cfg(test)] mod tests` blocks colocated with the code, so the filter strings below are module/function-name substrings, not separate file names.

| Layer | Command |
|---|---|
| `delta_summary` unit (decoder/classifier/projection) | `cargo test -p guardian-server delta_summary` |
| Listing service inline tests | `cargo test -p guardian-server dashboard_account_deltas dashboard_global_deltas` |
| Detail service inline tests | `cargo test -p guardian-server dashboard_account_delta_detail` |
| HTTP handler tests (route wiring, 400/404 shapes) | `cargo test -p guardian-server dashboard_feeds` |
| TS operator client | `npm test --workspace @openzeppelin/guardian-operator-client -- http.test.ts` |
| TS operator smoke | invoke skill `smoke-test-operator-dashboard` per its README |

## Acceptance signoff

For each story:
- US1 (P1): listing endpoints carry the new fields on the seeded mix-of-categories account; existing TS tests still pass.
- US2 (P2): detail endpoint returns the structured projection for at least the `p2id`, `consume_notes`, `add_signer`, and a custom delta; `?include=scripts` and `?include=raw` work; partial-decode case emits `decode_warnings[]`.
- US3 (P2): listing → detail round-trip succeeds; URL malformed cases return `400`; unknown/wrong-account/unauthorized cases return identical `404` bodies.
