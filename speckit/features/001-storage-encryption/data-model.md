# Phase 1 Data Model: Storage Encryption at Rest

This feature makes no changes to the existing payload tables or columns. It
changes the *value* representation of three existing payload fields, adds one
small encryption marker (a single-row Postgres table / a filesystem file, and
only in encrypted mode), and introduces in-memory types for the cipher, key
provider, and envelope.

## Affected stored payloads (existing types, unchanged shape)

| Type | Field (encrypted) | Plaintext routing/index fields kept as-is |
|---|---|---|
| `StateObject` (`crates/server/src/state_object.rs:9`) | `state_json: serde_json::Value` | `account_id`, `commitment`, `created_at`, `updated_at`, `auth_scheme` |
| `DeltaObject` (`crates/server/src/delta_object.rs:133`) | `delta_payload: serde_json::Value` | `account_id`, `nonce`, `prev_commitment`, `new_commitment`, `ack_sig`, `status` |
| Proposal (`DeltaObject` in `delta_proposals`) | `delta_payload` | `account_id`, `nonce`, `commitment` (`ProposalRecord.commitment`), `status` |

When encryption is enabled, the encrypted field holds an **Envelope** (below)
serialized as a `serde_json::Value`. The `jsonb` column type and filesystem
JSON file format are unchanged. All other fields remain plaintext and continue
to back every existing filter/sort/lookup (SC-003).

**Proposal read shape (trait change, see research R8)**: to reconstruct the
proposal AAD (`proposal:<account_id>:<commitment>`), the `commitment` must travel
with every proposal read. `pull_all_delta_proposals` and `pull_pending_proposals`
therefore return `Vec<ProposalRecord>` (which carries `commitment`) instead of
`Vec<DeltaObject>`, matching the already-`ProposalRecord` paginated reads.

## Envelope

The at-rest representation of one encrypted payload.

| Field | Type | Meaning |
|---|---|---|
| `v` | integer | Envelope scheme version (v1 = `1`). Enables future format changes. |
| `alg` | string | AEAD algorithm, e.g. `"AES-256-GCM"`. Enables algorithm agility. |
| `kid` | string | Key identifier the ciphertext was produced under (rotation/lookup). |
| `nonce` | string (base64) | Per-encryption random AEAD nonce (96-bit for AES-GCM). Unique per encryption (FR-007). |
| `ct` | string (base64) | Ciphertext with appended authentication tag. |

**Validation / invariants**:
- A value is a valid Envelope iff it is a JSON object containing `v`, `alg`,
  `kid`, `nonce`, `ct` with the expected types.
- `nonce` MUST be freshly random for every encryption; never reused under one key.
- In an encryption-enabled deployment, a payload that fails Envelope validation
  on read is an error (FR-010) — not returned as plaintext.
- The Envelope carries no plaintext fragment of the payload.

## Record identity / AAD

Additional Authenticated Data binds a ciphertext to the unique identity of its
record, so it cannot be relocated to another record undetected (FR-005).

| Record | AAD string | Uniqueness source |
|---|---|---|
| Account state | `state:<account_id>` | one state per account |
| Delta | `delta:<account_id>:<nonce>` | `(account_id, nonce)` UNIQUE on `deltas` |
| Proposal | `proposal:<account_id>:<commitment>` | `(account_id, commitment)` UNIQUE on `delta_proposals` (nonce is NOT unique here) |

The AAD is authenticated but not encrypted; it must be reconstructable from the
record's plaintext routing fields at decrypt time. Decrypting with a mismatched
AAD fails (tamper/relocation detected).

## StorageKey

| Aspect | Decision |
|---|---|
| Size | 32 bytes (256-bit) |
| Holder | `crate::secret::FixedKey<32>` (zeroize-on-drop) |
| Source | `StorageKeyProvider` (see contracts) |

## StorageKeyProvider (in-memory)

A provider holds a map of `kid → StorageKey` plus a designated **active `kid`**.
Keys are loaded **once at startup** and cached for the process lifetime (no
per-operation key-store calls).

| Implementation | Selected when | Key source / `kid` |
|---|---|---|
| `EnvKeyProvider` | dev / `GUARDIAN_STORAGE_ENCRYPTION_KEY` set | base64-encoded 32 bytes; `kid` from `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` (default fixed literal) |
| `SecretsManagerKeyProvider` | `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` set | AWS Secrets Manager structured secret (below), reuses ACK pattern |

**Production secret shape** (Secrets Manager `SecretString`): a structured
document supporting rotation —

```json
{ "active": "<kid>", "keys": { "<kid>": "<base64 32 bytes>", "<older-kid>": "<base64 32 bytes>" } }
```

`active` is the `kid` stamped on new writes; `keys` supplies every `kid` records
may reference (FR-011). The `kid` is an application-defined label, not the
Secrets Manager secret id or version id (see research R5).

Each provider exposes:
- `active_key_id() -> &str` — the `kid` stamped on new envelopes.
- `key(kid) -> StorageKey` — resolve the key for a given envelope `kid`
  (supports records written under prior keys; unknown `kid` is an error, FR-011).

## Encryption marker (FR-015)

A small, store-level record written **only in encrypted mode** when the store is
first initialized, and verified at startup before any write. Its *presence* means
the store is encrypted; plaintext (no-key) deployments write no marker, so a
fresh plaintext store is byte-for-byte identical to baseline (SC-008).

| Field | Meaning |
|---|---|
| `scheme_version` | envelope `v` at init |
| `init_kid` | key id the store was initialized under — informational/lineage only; does NOT pin the current active key (rotation may advance it, R5) |

Storage: filesystem → a dedicated marker file under the storage root; Postgres →
a single-row marker/settings table (the **only** schema addition; payload tables
unchanged). Startup logic per research R6 (**emptiness** = no state/delta/proposal payload
records; the marker/settings record is never counted):
- encrypted mode: absent+empty → write marker; absent+non-empty → fail fast;
  present → provider must contain `init_kid` (lineage) else fail fast;
- plaintext mode: never write; **present → fail fast (regardless of record
  count)**.

## Configuration (env vars)

| Variable | Role | Meaning |
|---|---|---|
| `GUARDIAN_STORAGE_ENCRYPTION_KEY` | dev key source | base64-encoded 32-byte key |
| `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` | dev (optional) | `kid` for the dev key (default fixed literal) |
| `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` | prod key source | Secrets Manager secret id (structured secret) |
| `AWS_REGION` | prod | already required by ACK; reused |

Enablement is inferred from key-source presence (FR-013): there is **no enable
flag**.
- 0 key sources configured → encryption off (plaintext, default).
- exactly 1 key source configured → encryption on.
- more than 1 key source configured → startup error (ambiguous).

Startup validation (FR-009): when a key source is configured, it MUST resolve a
valid 32-byte key, else fail fast — never silent plaintext.

## State transitions

None. Encryption does not alter the delta/proposal lifecycle
(pending → candidate → canonical → discarded) or append-only semantics; it only
changes how the payload field is stored and read back.
