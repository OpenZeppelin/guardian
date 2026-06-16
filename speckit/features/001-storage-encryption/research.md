# Phase 0 Research: Storage Encryption at Rest

All Technical Context unknowns are resolved below. Each item records the
decision, rationale, and alternatives considered.

## R1. AEAD algorithm and crate

**Decision**: AES-256-GCM via the `aes-gcm = "0.10"` crate (RustCrypto). The
envelope records the algorithm name so an alternative can be added later without
a format change.

**Rationale**: AES-256-GCM is a standard, widely-reviewed AEAD; it gives
confidentiality + integrity in one primitive (tamper detection for FR-004).
RustCrypto's `aes-gcm` is pure-Rust, already aligns with the existing RustCrypto
deps in the crate (`sha2`, `sha3`, `hmac`, `subtle`), and integrates with
`zeroize`. 96-bit random nonce per encryption (FR-007), 256-bit key (FR-008).

**Alternatives considered**:
- *ChaCha20-Poly1305* (`chacha20poly1305`): equally good; AES-GCM chosen for
  ubiquity and hardware acceleration on the target (x86-64 ECS). The envelope's
  `alg` field keeps this swappable.
- *AWS KMS `Encrypt`/`Decrypt` on the payload directly*: a network round-trip
  per row — unacceptable latency (SC-007) and a hard dependency on KMS
  availability for every read. Rejected for v1; KMS is reserved for the future
  key-wrapping provider (R4).
- *`ring` / `aws-lc-rs`*: capable but adds a heavier/native dependency not
  currently in the tree.

## R2. Envelope format and plaintext detection

**Decision**: Store each encrypted payload as a self-describing JSON object in
the *same* `jsonb` column / JSON file the plaintext used:

```json
{ "v": 1, "alg": "AES-256-GCM", "kid": "<key id>", "nonce": "<b64>", "ct": "<b64 ciphertext||tag>" }
```

A payload is recognized as encrypted iff it is a JSON object carrying the
envelope discriminator (`v` + `alg` + `ct`). In an encryption-enabled
deployment, a payload that is *not* a valid envelope is a read error (FR-010,
FR-014) — never returned as plaintext.

**Rationale**: Keeping the envelope as JSON means **no migration of the payload
tables** — the columns stay `jsonb` and filesystem files stay JSON; the decorator
only swaps the value of the payload field. (The feature's only schema addition is
the small encryption marker table/file from R6 — not a payload-table change.)
`v` enables scheme evolution; `kid` enables rotation (R5); `nonce` travels with
the ciphertext; base64 keeps it valid JSON.
The discriminator gives an unambiguous plaintext-vs-ciphertext check supporting
the fail-closed reads the constitution requires.

**Alternatives considered**:
- *Migrate columns to `bytea`*: cleaner typing but forces a schema migration and
  changes the not-found/error heuristics; rejected for larger blast radius and
  no functional gain.
- *Sidecar columns* (separate nonce/kid columns): spreads one logical value
  across the schema and needs migrations on every table; rejected.

## R3. Where the encryption boundary lives

**Decision**: A `EncryptedStorage` decorator implementing `StorageBackend`,
inserted in `StorageMetadataBuilder::build()` at the two `Arc::new(storage)`
sites (`crates/server/src/builder/storage.rs:118` Postgres,
`:134` filesystem). When encryption is disabled the concrete backend is returned
directly (zero overhead, SC-008).

**Rationale**: One chokepoint (FR-012) means both backends inherit encryption
identically, guaranteeing the "same observable semantics across backends"
invariant by construction, and every current/future caller is covered without
per-call-site handling. The trait surface is unchanged except for the
proposal-read return-type change required for AAD reconstruction (R8).

**Method-dispatch subtleties** (must be honored by the decorator):
- **Write** — clone the object, replace the payload field with its envelope,
  delegate: `submit_state`, `submit_delta`, `submit_delta_proposal`,
  `update_delta_proposal`.
- **Read** — delegate, then replace each returned object's payload field with the
  decrypted value: `pull_state`, `pull_delta`, `pull_deltas_after`,
  `pull_delta_proposal`, `pull_all_delta_proposals`, `list_account_deltas_paged`,
  `list_account_proposals_paged` (decrypt `ProposalRecord.proposal`),
  `list_global_deltas_paged` (decrypt `GlobalDeltaRow.delta`),
  `list_global_proposals_paged`.
- **Override to preserve backend optimization** — `pull_states_batch`: call
  `inner.pull_states_batch` (keeps the single Postgres round-trip), then decrypt
  each value. Do *not* fall back to the trait default (which would degrade to
  sequential `pull_state`).
- **Inherit the trait default (no override)** — `has_pending_candidate`,
  `pull_canonical_deltas_after`, `pull_pending_proposals`: these dispatch via
  `self` to a required read method the decorator already decrypts, so inheriting
  is correct and avoids double-decryption.
- **Pass-through (no payload touched)** — `kind`, `update_delta_status` (status
  columns only; the stored payload stays as its already-written envelope),
  `delete_delta`, `delete_delta_proposal`, `count_deltas_by_status`,
  `count_in_flight_proposals`, `latest_activity_timestamp`.

**Alternatives considered**:
- *Encrypt inside each backend (Postgres + filesystem separately)*: duplicates
  logic, risks drift between backends, violates the single-chokepoint goal.
- *Encrypt at the service layer*: leaks crypto concerns upward and breaks the
  "upper layers unchanged" requirement (SC-004).

## R4. Key provider and reuse of ACK Secrets Manager plumbing

**Decision**: A `StorageKeyProvider` trait with two v1 implementations:
- **Dev**: `EnvKeyProvider` — reads `GUARDIAN_STORAGE_ENCRYPTION_KEY`
  (base64-encoded 32 bytes), wrapped in `FixedKey<32>` from `crate::secret`.
- **Prod**: `SecretsManagerKeyProvider` — fetches the key from AWS Secrets
  Manager, reusing the pattern in `crates/server/src/ack/secrets_manager.rs`
  (`aws_config::defaults(...).load()`, `aws_sdk_secretsmanager::Client`,
  `get_secret_value`, `resolve_secret_id(env, default)`), selected by
  `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID`.

The key material is held in the existing zeroize-on-drop `crate::secret`
wrappers. The provider also exposes the `active_key_id` used to stamp new
envelopes.

**Rationale**: Secrets Manager is already the production secret store for ACK
keys; reuse keeps one auth/region/IAM story and avoids raw keys in process
config or logs. `aws-sdk-kms` is already a workspace dependency, so the future
KMS-wrapped-DEK / enclave provider (Out of Scope) can be added behind the same
trait with no new dependency and no envelope-format change (the `kid`/`alg`
fields already accommodate it).

**Runtime/caching model**: keys are fetched **once at startup** and held in the
zeroize-on-drop `crate::secret` wrappers for the process lifetime. `encrypt`/
`decrypt` never call the key store — they read from the in-memory provider. This
resolves the apparent tension between "no per-row network calls for latency"
(R1, SC-007) and "runtime key-store unavailability": the key store is a
**startup dependency**, not a per-operation one, so steady-state crypto does not
depend on its availability. If the store is unreachable at startup, the server
fails to start (FR-009). An unknown `kid` encountered at runtime (only possible
across an out-of-band rotation) is an explicit error, not a silent per-row
fetch; pre-loading all active+historical keys at startup avoids it.

**Alternatives considered**:
- *AWS Parameter Store*: also fine, but Secrets Manager is what ACK already uses
  — consistency wins.
- *KMS-wrapped DEK now*: stronger (key non-extractable) but it is the enclave/
  private-mode property the spec explicitly defers; v1 ships the raw-key path
  behind a seam that allows the upgrade later.

## R5. Key identity and rotation

**Decision**: Every envelope records `kid` (key id) and `v` (scheme version).
The provider holds a map of `kid → key` plus a designated **active `kid`** used
for new writes; reads resolve the key by the envelope's `kid`, so records written
under any held key decrypt (FR-011). Concretely:
- **Dev**: `GUARDIAN_STORAGE_ENCRYPTION_KEY` (one key); its `kid` comes from an
  optional `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` (default fixed literal, e.g.
  `"k1"`).
- **Prod**: the Secrets Manager secret is a **structured JSON document**:
  `{ "active": "<kid>", "keys": { "<kid>": "<base64 32 bytes>", ... } }`. v1 typically has
  one entry; rotation adds entries and moves `active`. The `kid` is therefore an
  application-defined label inside the secret — *not* the Secrets Manager secret
  id and *not* the SM version id (both are operationally awkward to use as the
  durable envelope key label).

v1 ships single-active-key encryption with multi-key *read* support. Active
re-encryption/rotation tooling is **out of scope** (recorded in spec).

**Rationale**: A structured secret with explicit `kid` labels makes multi-key
coexistence (FR-011) real and rotation a config change, while keeping the
envelope independent of AWS-specific identifiers. Recording identity now is
nearly free and avoids a flag-day migration later.

**Alternatives considered**:
- *`kid` = Secrets Manager version id*: couples the durable on-record label to an
  AWS artifact; restoring/migrating the secret could orphan records. Rejected.
- *`kid` = secret id*: only ever one value, defeats multi-key reads. Rejected.

## R6. Rollout and opt-in semantics

**Decision**: Encryption is opt-in by **key-source presence only** — there is no
separate enable flag (FR-013). With no key source configured, the system stores
plaintext (default, unchanged behavior). Configuring exactly one key source
turns encryption on; configuring more than one fails fast as ambiguous. It is
fixed for a populated store (FR-014). The standard production enablement happens
against the empty store produced by the Miden 0.15 cutover, so every record is
encrypted from the first write — no dual-read, no backfill. Switching state on a
non-empty store is a separate, explicit re-encryption migration (out of scope).

**Rationale**: A separate boolean flag is redundant with key presence and
introduces a genuinely dangerous combination — "flag says on, key absent" —
whose only safe resolution is the same fail-fast we already require. Inferring
intent from "did you configure a key source" removes that ambiguity and shrinks
the config surface, which matters for a security feature. The critical guarantee
is preserved by **fail-closed startup**, not by the flag: a configured-but-
unresolvable/invalid key is a hard startup error (FR-009), never a silent
fall-through to plaintext. (A purpose-named key var is not set by accident, so
inference does not meaningfully increase the risk of unintended enablement; if a
deployment wants an explicit assurance, the right place is a deploy-time
assertion that the secret exists, not a runtime boolean that can disagree with
reality.)

**Guard (mandatory — FR-015)**: lazy on-read detection (FR-010) is *not*
sufficient, because new encrypted writes could land in a plaintext store before
any read happens. The store therefore carries a persisted **encryption marker**.
To keep SC-008 (no-key mode is byte-for-byte baseline) intact, the marker is
written **only in encrypted mode**; its *presence* means the store is encrypted.
**Emptiness** is defined precisely: a store is empty when it holds **no
state/delta/proposal payload records**. The marker/settings record itself is
never counted — so a freshly-initialized encrypted store (marker present, zero
payload records) is still "empty".

Checked at startup before any write:
- **Encrypted mode** (key configured): marker absent + store empty → write the
  marker (records scheme version + the init `kid`); marker absent + store
  non-empty → **fail fast** (could be a legacy plaintext store); marker present →
  the provider MUST contain the marker's recorded `init_kid` (a lineage check)
  else **fail fast**.
- **Plaintext mode** (no key): writes **no** marker (SC-008); marker present →
  **fail fast regardless of record count** (refuse to write plaintext into a
  store marked encrypted, including a marker-only empty encrypted store);
  otherwise run normally.
- **Rotation does not trip the guard**: the marker's `init_kid` is the key the
  store was *initialized* under and is informational/lineage — it does **not**
  pin the current active `kid`. Because the structured secret retains historical
  keys (R5), the provider still contains `init_kid` after the active key advances
  (k1 → k2), so the lineage check passes. Per-record unknown-`kid` errors
  (FR-010, R8) remain the correctness backstop for any genuinely absent key.

**Marker storage**: the marker is small and read once at startup.
- *Filesystem*: a dedicated marker file under the storage root.
- *Postgres*: a single-row marker/settings table. This is the **only** schema
  addition — the existing payload tables (`states`, `deltas`,
  `delta_proposals`) are untouched (no column/type migration). Reuse the
  metadata store if it already offers a settings row; otherwise a minimal
  one-row table via an additive migration.

The on-read mismatch error (FR-010) remains as defense-in-depth.

## R7. Error surface

**Decision**: Decryption failures (wrong key, tamper, AAD/identity mismatch,
unknown `kid`, malformed/absent envelope) return the existing
`Result<_, String>` storage error; the service layer maps them to the same
boundary error it already returns for storage read failures. No new external
error code.

**Rationale**: Preserves stable boundary error semantics (Constitution IV); a
read that cannot be decrypted is operationally a failed read, not a new
user-facing category.

## R8. Proposal reads must carry the commitment (trait change)

**Decision**: Change `pull_all_delta_proposals` and `pull_pending_proposals` to
return `Vec<ProposalRecord>` instead of `Vec<DeltaObject>`, so every proposal
read path carries the `commitment` needed to reconstruct the proposal AAD
(`proposal:<account_id>:<commitment>`, FR-005).

**Rationale**: The proposal AAD binds to `(account_id, commitment)` because
`(account_id, nonce)` is **not** unique on `delta_proposals`. But the current
`pull_all_delta_proposals` returns bare `DeltaObject`
(`crates/server/src/storage/mod.rs:227`), and `DeltaObject` has no proposal
`commitment` field (`crates/server/src/delta_object.rs`, only `prev_commitment` /
`new_commitment`). So the decorator cannot rebuild the AAD to decrypt these
records. `ProposalRecord` already carries `commitment` and is already returned by
the paginated proposal reads (`list_account_proposals_paged`,
`list_global_proposals_paged`); extending the two non-paginated reads to match is
the consistent fix.

**Propagation (Constitution I)**: non-test consumers to update —
`crates/server/src/services/get_delta_proposals.rs:33`,
`crates/server/src/services/push_delta_proposal.rs:96` (via
`pull_pending_proposals`), `crates/server/src/evm/service.rs:269`, and the
filesystem backend's internal callers (`filesystem.rs:877,897`). These mostly
need `.proposal` to reach the inner `DeltaObject`; `MockStorageBackend` and tests
update accordingly.

**Alternatives considered**:
- *Bind proposal AAD to `account_id` + `nonce`*: not unique → swap protection
  lost. Rejected (this is the exact catch FR-005 encodes).
- *Bind to `new_commitment` (present on `DeltaObject`)*: it is `Option` and not
  guaranteed equal to the storage `commitment` cosigners sign; fragile. Rejected.
- *Enumerate commitments inside the decorator*: the bare-`DeltaObject` return
  discards them, so this still requires the backend to surface commitments — i.e.
  the same trait change. Rejected as a non-fix.

## R9. Service-path reads must be fail-closed

**Decision**: Remove the error-swallowing `.unwrap_or_default()` at
`crates/server/src/services/get_delta_proposals.rs:35` so a storage/decryption
error propagates as a failed request rather than an empty proposal list. Add
service-path error-propagation tests.

**Rationale**: FR-010 requires an undecryptable payload to make the read fail.
`unwrap_or_default()` turns a decrypt error from `pull_all_delta_proposals` into
`[]`, silently hiding it and presenting "no proposals" — a correctness and
security regression. (The other `unwrap_or_default()` at
`push_delta_proposal.rs:123` is on a `signatures` JSON array, not a storage read,
and is unaffected.) A scan for `unwrap_or_default()`/`ok()` on storage reads is
part of the work to ensure no other read path swallows errors.
