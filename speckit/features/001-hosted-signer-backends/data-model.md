# Phase 1 Data Model: Modular Hosted ECDSA Signer Backends

This feature introduces **no persisted data** (no DB tables, no new delta/proposal
records). The "model" here is the in-process type structure of the ECDSA signing
abstraction.

## Types

### `EcdsaSignerBackend` (trait — the abstraction)

The contract the ECDSA ack path signs through. Object-safe, async.

| Member | Signature | Notes |
|---|---|---|
| `public_key` | `fn public_key(&self) -> PublicKey` | The secp256k1 public key this backend signs with. Used once to derive `pubkey_hex` and `commitment`. |
| `sign` | `async fn sign(&self, message: Word) -> Result<Signature>` | Returns a Miden 65-byte `r\|\|s\|\|v` signature over `Keccak256(message)` such that `public_key().verify(message, &sig)` holds. |
| `backend_id` | `fn backend_id(&self) -> &'static str` | Stable identifier (`"in-memory"`, `"aws-kms"`) for logging/diagnostics. |

**Invariants**:
- `sign` output MUST verify against `public_key()` and serialize to exactly 65 bytes.
- MUST NOT log or expose private key material; only `public_key()` is surfaced.

### `InMemoryEcdsaBackend` (implements `EcdsaSignerBackend`)

Wraps the existing `FilesystemEcdsaKeyStore` + server pubkey `Word`. Behavior is
today's `MidenEcdsaSigner` signing logic, unchanged. `sign` is synchronous work
wrapped in the async trait. **The `ZeroizeOnDrop` rationale comment currently at
`miden_ecdsa/signer.rs:51-57` moves here verbatim** — it is the security basis for
the default path and must survive the refactor.

| Field | Type | Source |
|---|---|---|
| `keystore` | `Arc<FilesystemEcdsaKeyStore>` | filesystem, or imported from AWS Secrets Manager when `GUARDIAN_ENV=prod` (existing path) |
| `server_pubkey_word` | `Word` | keystore key handle |
| `public_key` | `PublicKey` | derived from keystore |

### `KmsEcdsaClient` (internal seam — implements provider-agnostic testing)

A narrow internal trait so `AwsKmsEcdsaBackend` does not depend directly on the
AWS SDK concrete type, enabling deterministic unit tests (failures, delays,
permission denials) without real AWS.

| Member | Signature | Notes |
|---|---|---|
| `get_public_key` | `async fn get_public_key(&self) -> Result<KmsPublicKeyInfo>` | SPKI/DER pubkey + key spec + usage for startup validation |
| `sign` | `async fn sign(&self, digest: [u8;32]) -> Result<Vec<u8>>` | DER signature over the supplied digest (DIGEST mode) |

Real impl wraps `aws_sdk_kms::Client`; tests use a fake backed by a local
secp256k1 key (or programmed to error/delay).

### `AwsKmsEcdsaBackend` (implements `EcdsaSignerBackend`)

Holds only a key handle; private key never enters the process.

| Field | Type | Source |
|---|---|---|
| `client` | `Arc<dyn KmsEcdsaClient>` | real: `aws_sdk_kms::Client` w/ explicit `TimeoutConfig`; test: fake |
| `key_id` | `String` (non-secret handle — no `crate::secret` wrapper) | `GUARDIAN_ACK_ECDSA_KMS_KEY_ID` |
| `public_key` | `PublicKey` | `get_public_key` at construction (validated `ECC_SECG_P256K1`) |

Built **once at boot** and `Arc`-shared across per-request `AppState`/`AckRegistry`
clones so the SDK connection pool is reused (never reconstruct the client in
`sign`). No memory-resident secret → no zeroize wrappers needed; `Debug` is
non-disclosing. (See plan.md "Operational concerns".)

**Construction (`connect`) validates fail-fast (FR-007):**
1. `get_public_key` → assert key spec `ECC_SECG_P256K1` + sign-capable usage; derive `public_key`, `pubkey_hex`, `commitment_hex`.
2. **Sign probe** → `sign` a fixed validation `Word` constant and assert the converted signature verifies against `public_key`. This proves `kms:Sign` (which `get_public_key` cannot) and exercises the conversion path at boot.
3. Any failure → `ConfigurationError` naming the cause.

The real client carries an explicit `TimeoutConfig` (bounded
per-operation/attempt timeout, value pinned in tasks) so a hung call fails the ack
rather than hanging (FR-009 / spec latency edge case).

`sign` flow: `Keccak256(word)` → `client.sign(digest)` → DER →
`Signature::from_der` + recovery-id resolution via `PublicKey::recover_from`
(see research.md R2).

### `build_ecdsa_backend` (async factory / registration point)

Resolves the configured backend id to a constructed `Arc<dyn EcdsaSignerBackend>`.
This is the single place a contributor edits to register a new provider (FR-010,
SC-005). It is **`async`** because hosted backends await client construction and
startup key validation.

| Input | Type | Notes |
|---|---|---|
| backend id | `&str` from `GUARDIAN_ACK_ECDSA_BACKEND` | default `"in-memory"` when unset |
| prod | `bool` | from `is_prod_environment()`; only consulted on the in-memory path |
| keystore_path | `PathBuf` | filesystem keystore location for the in-memory path |

State transitions (startup):
- unset / `"in-memory"` → `InMemoryEcdsaBackend` (existing filesystem rule, or Secrets-Manager import when `prod`).
- `"aws-kms"` → `AwsKmsEcdsaBackend` (requires key id + region; validates key spec & permission; **never** touches Secrets Manager).
- unknown id → **fail fast** (`ConfigurationError`), error lists supported ids.

### `AckRegistry::new` (restructured — resolves the prod/KMS construction gap)

Builds Falcon and ECDSA **independently** so the ECDSA secret is fetched only when
the ECDSA backend is in-memory. Falcon keeps following `GUARDIAN_ENV`; ECDSA
follows `build_ecdsa_backend`. With `GUARDIAN_ENV=prod` + `aws-kms`, the Falcon
secret is fetched from Secrets Manager but `ecdsa_secret_key()` is **not** called.
(See plan.md "AckRegistry construction flow".)

### `MidenEcdsaSigner` (reshaped wrapper)

| Field | Type |
|---|---|
| `backend` | `Arc<dyn EcdsaSignerBackend>` |
| `pubkey_hex` | `String` (cached from `backend.public_key()`) |
| `commitment_hex` | `String` (cached) |

Public-ish surface preserved: `pubkey_hex()`, `commitment_hex()` (sync);
`ack_delta(delta)` becomes `async` and awaits `backend.sign`.

### `AckRegistry` (unchanged surface except async)

Holds `falcon: MidenFalconRpoSigner` (unchanged) and `ecdsa: MidenEcdsaSigner`.
`pubkey(scheme)` / `commitment(scheme)` stay sync; `ack_delta(delta, scheme)`
becomes `async`.

## Error model

**No new error enum.** The trait returns `crate::error::Result` (= `GuardianError`);
hosted failures reuse the two existing variants, which already carry stable
HTTP/gRPC/code/Display mappings (`error.rs`). `MidenEcdsaError` is **not** extended.

- **Startup/config** → `GuardianError::ConfigurationError` (fail-fast at boot;
  HTTP 500 / gRPC `Internal` / `"configuration_error"`): unknown backend id,
  missing `GUARDIAN_ACK_ECDSA_KMS_KEY_ID`, KMS key not found, wrong key spec,
  missing `kms:GetPublicKey`/`kms:Sign` permission.
- **Runtime sign failure** → `GuardianError::SigningError` (HTTP 500 / gRPC
  `Internal` / `"signing_error"`): transient KMS error, **timeout**, recovery-id
  resolution failure. Surfaces as a clear error for that ack; server stays
  healthy; no unsigned/placeholder ack (FR-009).

## External entities (not owned)

- **`miden_protocol::crypto::dsa::ecdsa_k256_keccak::{SecretKey, PublicKey, Signature}`** — scheme types; used read-only via public API.
- **AWS KMS asymmetric key** — `ECC_SECG_P256K1`, `SIGN_VERIFY`. Provisioned and rotated out of band by the operator.
