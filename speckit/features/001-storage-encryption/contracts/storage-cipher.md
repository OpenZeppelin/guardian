# Internal Contracts: Storage Cipher, Key Provider, Decorator

This feature exposes **no external API** (no HTTP/gRPC endpoint). The contracts
below are the internal Rust trait boundaries the implementation must honor, in
`crates/server/src/storage/encryption/`.

## `StorageKeyProvider`

```rust
#[async_trait]
pub trait StorageKeyProvider: Send + Sync {
    /// `kid` stamped onto envelopes for new writes.
    fn active_key_id(&self) -> &str;

    /// Resolve the 32-byte key for a given envelope `kid`.
    /// Unknown `kid` MUST return an error (FR-011), never a wrong key.
    async fn key(&self, kid: &str) -> Result<FixedKey<32>, KeyProviderError>;
}
```

Implementations (v1):
- `EnvKeyProvider` — `GUARDIAN_STORAGE_ENCRYPTION_KEY` (base64-encoded 32 bytes).
- `SecretsManagerKeyProvider` — `GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID`,
  reusing `crates/server/src/ack/secrets_manager.rs` (region, client,
  `get_secret_value`, `resolve_secret_id`).

Contract:
- A provider holds a `kid → key` map plus an active `kid`; keys are loaded once
  at construction (startup) and cached for the process lifetime — `key()` does
  not call the key store per invocation (R4).
- Each key MUST decode (base64) to exactly 32 bytes; wrong length / malformed
  base64 / missing secret / unparsable structured secret is an error (drives
  startup fail-fast, FR-009). More than one key source configured is also an error.
- Prod secret is the structured document `{ "active": kid, "keys": { kid: base64-32-bytes } }`
  (R5). Dev `kid` from `GUARDIAN_STORAGE_ENCRYPTION_KEY_ID` (default literal).
- Returned key is held in a zeroize-on-drop wrapper (`FixedKey<32>`).

### Error variants (diagnosability)

Distinct, non-leaking variants (mirroring the OZ relayer's design) so failures
are diagnosable without exposing key/plaintext material:

```rust
pub enum KeyProviderError {
    MissingKeySource,        // none configured (caller treats as "encryption off")
    MultipleKeySources,      // >1 configured — ambiguous (FR-009)
    InvalidKeyEncoding,      // base64 decode failed
    InvalidKeyLength,        // decoded != 32 bytes
    MalformedSecret,         // structured secret unparsable / missing `active`/`keys`
    UnknownKeyId(String),    // envelope `kid` not in provider (FR-011)
    KeyStoreUnavailable,     // Secrets Manager fetch failed at startup
}

pub enum CipherError {
    NotAnEnvelope,           // payload is not a valid envelope (FR-010)
    UnsupportedVersion(u8),  // envelope `v` not understood
    UnsupportedAlgorithm,    // envelope `alg` not understood
    InvalidNonce,            // nonce wrong length / undecodable
    DecryptionFailed,        // AEAD auth/tag failure: tamper, wrong key, or AAD mismatch
    KeyProvider(KeyProviderError), // propagated key resolution failure (e.g. UnknownKeyId)
}
```

These convert into the storage `Result<_, String>` at the decorator boundary and
map to the existing storage boundary error at the service layer (Constitution IV).

## `StorageCipher`

```rust
pub trait StorageCipher: Send + Sync {
    /// Encrypt a payload, binding it to `aad` (record identity).
    fn encrypt(&self, aad: &RecordAad, plaintext: &serde_json::Value)
        -> Result<serde_json::Value /* Envelope */, CipherError>;

    /// Decrypt an envelope, requiring the same `aad`.
    fn decrypt(&self, aad: &RecordAad, envelope: &serde_json::Value)
        -> Result<serde_json::Value, CipherError>;
}
```

`Aes256GcmCipher` (v1) holds a `StorageKeyProvider`. Contract:
- `encrypt` generates a fresh random 96-bit nonce per call (FR-007), produces an
  Envelope per `contracts/envelope.schema.json`, stamps `active_key_id()`.
- `decrypt` validates the envelope shape, resolves the key by `kid`, verifies the
  AAD and authentication tag; any failure (tamper, wrong key, AAD mismatch,
  unknown `kid`, malformed envelope) is a `CipherError` — no partial/plaintext
  result (FR-004, FR-010).

### `RecordAad`

```rust
pub enum RecordAad<'a> {
    State { account_id: &'a str },                       // "state:{account_id}"
    Delta { account_id: &'a str, nonce: u64 },           // "delta:{account_id}:{nonce}"
    Proposal { account_id: &'a str, commitment: &'a str },// "proposal:{account_id}:{commitment}"
}
```

The byte string above is the AEAD AAD. Built from plaintext routing fields, so it
is reconstructable at decrypt time without the key.

## `EncryptedStorage` decorator

```rust
pub struct EncryptedStorage {
    inner: Arc<dyn StorageBackend>,
    cipher: Arc<dyn StorageCipher>,
}
```

Implements `StorageBackend`. Per-method behavior (full trait surface from
`crates/server/src/storage/mod.rs`):

| Method | Behavior |
|---|---|
| `submit_state` | clone, encrypt `state_json` with `State{account_id}` AAD, delegate |
| `submit_delta` | clone, encrypt `delta_payload` with `Delta{account_id,nonce}` AAD, delegate |
| `submit_delta_proposal(commitment, proposal)` | clone, encrypt with `Proposal{account_id,commitment}` AAD, delegate |
| `update_delta_proposal(commitment, proposal)` | clone, encrypt with `Proposal{account_id,commitment}` AAD, delegate |
| `pull_state` | delegate, decrypt `state_json` |
| `pull_states_batch` | **override**: delegate to `inner.pull_states_batch` (keep batched round-trip), decrypt each |
| `pull_delta` | delegate, decrypt |
| `pull_deltas_after` | delegate, decrypt each |
| `pull_delta_proposal` | delegate, decrypt |
| `pull_all_delta_proposals` | **returns `Vec<ProposalRecord>`** (trait change, R8); delegate, decrypt each `.proposal` with `Proposal{account_id, commitment}` AAD |
| `list_account_deltas_paged` | delegate, decrypt each |
| `list_account_proposals_paged` | delegate, decrypt each `ProposalRecord.proposal` |
| `list_global_deltas_paged` | delegate, decrypt each `GlobalDeltaRow.delta` |
| `list_global_proposals_paged` | delegate, decrypt each `ProposalRecord.proposal` |
| `has_pending_candidate`, `pull_canonical_deltas_after`, `pull_pending_proposals` | **inherit trait default** — dispatch via `self` to a decrypting read method (no override, no double-decrypt). `pull_pending_proposals` now also returns `Vec<ProposalRecord>` (R8); its default `retain`/`sort` operate on `.proposal` |
| `update_delta_status` | pass-through (status columns only; stored envelope untouched) |
| `delete_delta`, `delete_delta_proposal` | pass-through |
| `count_deltas_by_status`, `count_in_flight_proposals`, `latest_activity_timestamp`, `kind` | pass-through |

Wiring (`builder/storage.rs`): enablement is inferred from key-source presence
(no enable flag). If exactly one of `GUARDIAN_STORAGE_ENCRYPTION_KEY` /
`GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID` is configured, resolve the matching
`StorageKeyProvider`, build `Aes256GcmCipher`, and return
`Arc::new(EncryptedStorage::new(inner, cipher))`. If none is configured, return
`inner` directly (zero overhead, SC-008). If more than one is configured, fail
fast (FR-009).

**Startup marker check (FR-015)** — before returning the backend:
- *Encrypted mode* (key configured): marker absent+empty → write marker (records
  `scheme_version` + `init_kid`); marker absent+non-empty → fail fast; marker
  present → provider MUST contain the marker's `init_kid` (lineage check — the
  *current* active kid may differ, so rotation is fine) else fail fast.
- *Plaintext mode* (no key): never write a marker (SC-008); marker present →
  fail fast regardless of record count (refuse plaintext into an encrypted-marked
  store).

Emptiness = no state/delta/proposal payload records (the marker/settings record
is not counted).

See data-model "Encryption marker" and research R6.

## Trait change (research R8)

`StorageBackend::pull_all_delta_proposals` and `pull_pending_proposals` change
return type from `Vec<DeltaObject>` to `Vec<ProposalRecord>` so the proposal
`commitment` is available to rebuild the AAD. Non-test consumers updated:
`services/get_delta_proposals.rs`, `services/push_delta_proposal.rs`,
`evm/service.rs`, filesystem internal callers; plus `MockStorageBackend`/tests
(Constitution I).

## Service-path fail-closed (FR-010, research R9)

`services/get_delta_proposals.rs` MUST propagate the storage `Result` instead of
`.unwrap_or_default()`, so a decryption failure becomes a failed request, not an
empty list. Add error-propagation tests; scan other read service paths for
error-swallowing `.unwrap_or_default()` / `.ok()`.

## Error mapping

`KeyProviderError` / `CipherError` convert into the existing storage
`Result<_, String>` error and map at the service layer to the same boundary
error already used for storage read failures (Constitution IV — no new external
error code).
