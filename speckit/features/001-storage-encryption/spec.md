# Feature Specification: Storage Encryption at Rest

**Feature Branch**: `001-storage-encryption`  
**Created**: 2026-06-16  
**Status**: Draft  
**Input**: User description: "Encrypt sensitive Guardian storage blobs (account state JSON, delta payloads, and proposal payloads) at rest using authenticated encryption with a pluggable 32-byte key provider, keeping routing and index fields plaintext, binding ciphertext to record identity, and decrypting transparently on load so upper layers are unchanged"

## Overview

Guardian persists account state and the deltas/proposals that mutate it as
plaintext JSON, in both the filesystem (development) and Postgres (production)
backends. The underlying disk is already encrypted at rest in production
(managed-key volume encryption), but that protects only against theft of
physical media or backups — anyone who can read the database (leaked
connection string, a misconfigured role, SQL injection, a malicious operator)
sees account contents in the clear.

This feature adds an application-layer confidentiality boundary: the sensitive
payloads are authenticated-encrypted before they are written to storage and
decrypted immediately after they are read back, using a key that lives outside
the database. The threat it closes is **an actor with live read access to the
storage layer but without the encryption key**. It is explicitly *not* a
defence against compromise of the running Guardian process (which holds the
key in memory in the base configuration); raising that bar is the separate
"private mode / enclave" effort and is out of scope here.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Account contents are unreadable from the database alone (Priority: P1)

A security engineer obtains a copy of the production database (a leaked
snapshot, a dump produced under an over-broad role, or rows returned through
an injection). The account state, deltas, and proposal payloads must be
unintelligible to them; the only thing they can learn is routing metadata
(which account, which nonce, which lifecycle status), not the account
contents themselves.

**Why this priority**: This is the entire point of the feature — moving the
trust boundary off the database host. Without it, none of the other stories
deliver value.

**Independent Test**: Provision Guardian with encryption enabled, write a
state and a delta, then read the raw stored rows/files directly (bypassing
Guardian). Confirm the sensitive payloads are ciphertext envelopes with no
recoverable plaintext, while `account_id`, `nonce`, and commitments remain
readable. Then read the same records back *through* Guardian with the correct
key and confirm the original objects are returned intact.

**Acceptance Scenarios**:

1. **Given** encryption is configured and an account state has been stored, **When** the underlying storage row/file is inspected directly, **Then** the state payload is an encrypted envelope and no field of the original account state is recoverable without the key.
2. **Given** an encrypted state and delta exist, **When** Guardian loads them with the correct key, **Then** the service layer receives the identical `StateObject` / `DeltaObject` it would have received from plaintext storage.
3. **Given** routing fields (`account_id`, `nonce`, `prev_commitment`, `new_commitment`, `status_kind`, timestamps) are needed for lookups, **When** records are stored encrypted, **Then** those fields remain in plaintext and all existing history, by-nonce, by-commitment, and status-filter reads continue to work unchanged.

---

### User Story 2 - Operators configure a key appropriate to their environment (Priority: P2)

An operator runs Guardian locally with a simple configured key for fast
iteration, and in production with a managed key source so the key material is
governed and not pasted into plain environment configuration where avoidable.

**Why this priority**: The protection from Story 1 is only real if the key is
sourced correctly per environment; a dev-only key path is insufficient for
production, and a production-only path blocks local development.

**Independent Test**: Start Guardian in development with a configured 32-byte
key and confirm encrypted read/write works end to end. Separately, start
Guardian configured against the managed key provider and confirm it sources
the key without the raw key appearing in process configuration.

**Acceptance Scenarios**:

1. **Given** a development environment, **When** a valid 32-byte key is provided through configuration, **Then** Guardian starts and performs encrypted storage operations.
2. **Given** a production environment configured to use the managed key provider (AWS Secrets Manager, the same secret store Guardian already uses for ACK keys), **When** Guardian starts, **Then** it obtains the key through that provider rather than a raw configuration value.
3. **Given** a key source is configured but the key cannot be resolved or is the wrong length / malformed, **When** Guardian starts, **Then** startup fails fast with a clear error rather than silently writing plaintext.

---

### User Story 3 - Records are tamper-evident and bound to their identity (Priority: P2)

The encryption must detect tampering and prevent an attacker who can write to
the database from silently substituting one account's encrypted payload for
another's, or rolling a record back to an older ciphertext for the same slot.

**Why this priority**: Confidentiality without integrity leaves a writable
database open to undetected swap/replay attacks; binding ciphertext to record
identity is what makes the at-rest guarantee meaningful against a writer, not
just a reader.

**Independent Test**: Encrypt a payload for one record, then attempt to load it
as if it belonged to a different account/nonce/commitment, and attempt to load
a bit-flipped ciphertext. Confirm both are rejected as errors rather than
returning data.

**Acceptance Scenarios**:

1. **Given** an encrypted payload bound to a record identity, **When** it is presented under a different record identity, **Then** decryption fails and the read is rejected.
2. **Given** an encrypted payload, **When** any byte of the envelope is altered, **Then** decryption fails and the read is rejected.
3. **Given** a record whose stored key identifier is unknown to the configured key provider, **When** it is loaded, **Then** the read fails with a clear error rather than returning corrupt or partial data.

---

### User Story 4 - Keys carry an identity that supports future rotation (Priority: P3)

Each encrypted record records which key (and envelope scheme version) produced
it, so that the encryption key can be rotated or the scheme upgraded over time
without a flag-day re-encryption of all historical data.

**Why this priority**: Rotation is an operational eventuality for any
long-lived key; recording key identity now is cheap and avoids a painful
migration later. Active rotation tooling itself is not required for the
initial release.

**Independent Test**: Store records under one key identity, then confirm the
stored envelopes carry that identity and scheme version, and that records
written under different identities can coexist and each be read with the
correct key.

**Acceptance Scenarios**:

1. **Given** a record is encrypted, **When** the envelope is stored, **Then** it carries a key identifier and a scheme version.
2. **Given** records exist under two different key identities, **When** both are read with a provider that knows both keys, **Then** each decrypts correctly.

---

### Edge Cases

- **Key source configured but unusable at startup**: a key source is set but the key cannot be resolved, is the wrong length, or is malformed → fail fast at startup, never fall back to plaintext.
- **More than one key source configured**: ambiguous which key to use → fail fast at startup.
- **Decryption failure on read**: wrong key, corrupted bytes, tampered envelope, or mismatched record identity → surface as a read error; the affected record is treated as unreadable rather than returning garbage.
- **Unknown key identifier**: an envelope references a key the provider cannot supply → explicit error.
- **Unexpected plaintext payload (encryption-enabled deployment)**: a deployment running with encryption enabled was populated from empty, so every record is encrypted from the first write; a non-enveloped (plaintext) payload encountered on read is therefore an error, not a supported fallback.
- **Mode change on a non-empty store**: enabling encryption on a deployment that already holds plaintext records (or disabling it on one holding ciphertext) is rejected/guarded rather than silently producing a mixed, partially-unreadable data set; switching requires an explicit re-encryption migration (out of scope).
- **Proposal payloads at the same nonce**: multiple proposals can share `(account_id, nonce)`, so record-identity binding for proposals must use the per-proposal unique identity (commitment), not the nonce, to keep swap-protection meaningful.
- **Key store reachable only at startup**: keys are resolved once at startup and held in memory for the process lifetime, so steady-state reads and writes do not call the key store per operation (preserving latency and avoiding a per-request dependency). If the key store is unreachable at startup, the server fails to start (fail-fast) rather than running without keys or exposing plaintext.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST encrypt the sensitive stored payloads — account state, delta payloads, and proposal payloads — before they are persisted, in both the filesystem and Postgres backends.
- **FR-002**: The system MUST decrypt these payloads immediately after they are loaded from storage, so that all layers above the storage boundary continue to operate on the same in-memory objects as today, with no changes required outside the storage boundary.
- **FR-003**: The system MUST keep routing and index fields in plaintext: `account_id`, `nonce`, `prev_commitment`, `new_commitment`, lifecycle status (`status_kind`), and timestamps. All existing reads that filter or sort on these fields MUST continue to work unchanged.
- **FR-004**: The system MUST use authenticated encryption so that any modification to a stored payload is detected on read and rejected.
- **FR-005**: The system MUST bind each ciphertext to the identity of the record it belongs to, such that a payload moved to a different record is rejected on read. Binding identity MUST be unique per record: account state by account, deltas by account and nonce, and proposals by account and commitment.
- **FR-006**: The system MUST store each encrypted payload as a self-describing envelope that includes the scheme version, the key identifier, the per-encryption nonce, and the ciphertext.
- **FR-007**: The system MUST generate a fresh, unique encryption nonce for every encryption operation; nonces MUST NOT be reused under the same key.
- **FR-008**: The system MUST obtain its encryption key from a configurable key provider, supporting at minimum (a) a directly-configured 32-byte key for development and (b) a managed key provider for production that does not require the raw key in plain process configuration. The production provider MUST be AWS Secrets Manager, reusing the existing ACK-key secret-sourcing path.
- **FR-009**: When a key source is configured, the system MUST resolve and validate the key at startup and fail fast with a clear error if the key cannot be resolved, is malformed, or is not exactly 32 bytes — it MUST NOT silently fall back to plaintext. Configuring more than one key source MUST also fail fast as ambiguous.
- **FR-010**: When a payload cannot be decrypted (wrong key, tampering, identity mismatch, or unknown key identifier), the system MUST treat the read as a failure and MUST NOT return partial, corrupt, or plaintext data.
- **FR-011**: The system MUST allow records encrypted under different key identifiers/scheme versions to coexist and be read correctly, provided the configured provider can supply the referenced keys.
- **FR-012**: The encryption boundary MUST be applied uniformly at the storage layer so that all current and future callers of the storage layer inherit it without per-call-site handling.
- **FR-013**: Storage encryption MUST be opt-in, determined solely by whether a key source is configured: with no key source configured the system stores plaintext and behaves exactly as it does today; configuring a key source enables encryption. There MUST be no separate enable/disable flag — providing a key is the act of opting in.
- **FR-014**: The encryption state MUST be consistent for a given populated data store. The system MUST NOT silently begin writing encrypted records into a store that already holds plaintext records, or vice versa, since that would produce an unreadable mixed data set. Changing the state of a non-empty store requires an explicit re-encryption migration (out of scope for this feature). On a store that is empty at first use — the state after the Miden 0.15 cutover — either may be selected. A store is **empty** when it holds no state/delta/proposal payload records; the encryption marker / settings record is never counted toward emptiness.
- **FR-015**: The system MUST enforce FR-014 with an explicit startup guard, not only by lazy detection on read, using a store-level encryption marker whose *presence* indicates the store is encrypted. **In encrypted mode** (a key is configured): on first initialization of an empty store the system MUST persist the marker (recording the scheme version and the key id the store was initialized under); at startup, before any write, it MUST fail fast if the marker is absent but the store is non-empty (a possible pre-feature/plaintext store) or if the key provider does not contain the marker's recorded key id. **In plaintext mode** (no key configured): the system MUST NOT write a marker (preserving byte-for-byte baseline storage, SC-008) but MUST fail fast at startup if a marker is present at all — regardless of record count — rather than write plaintext into a store marked encrypted. The marker's recorded key id is informational/lineage only and MUST NOT pin the current active key, so key rotation (FR-011) is unaffected.

### Key Entities

- **Sensitive payload**: the opaque, account-private content Guardian stores on behalf of a client — account state JSON and the delta/proposal payloads. Subject to encryption.
- **Routing/index fields**: non-secret identifiers and lifecycle metadata used to locate and order records (account, nonce, commitments, status, timestamps). Always plaintext.
- **Encrypted envelope**: the stored representation of a sensitive payload — scheme version, key identifier, per-encryption nonce, and ciphertext. The record identity it is bound to is *not* stored in the envelope; it is authenticated data reconstructed from the record's plaintext routing fields at decrypt time.
- **Key provider**: the source of the encryption key for an environment — a configured key for development, a managed provider for production — addressed by key identifier.
- **Record identity**: the unique handle a ciphertext is bound to so it cannot be relocated — account for state, account+nonce for deltas, account+commitment for proposals.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of newly written state, delta, and proposal payloads are stored as encrypted envelopes; a direct inspection of the storage layer recovers none of the original payload content without the key.
- **SC-002**: An actor with full read access to the storage layer but without the key can recover routing/index fields only, and 0% of account/delta/proposal payload content.
- **SC-003**: All existing storage reads that depend on routing/index fields (per-account history, single-record lookup by nonce, lookup by commitment/transaction id, and status filtering) return identical results with encryption enabled.
- **SC-004**: No code above the storage boundary changes to accommodate encryption; the service and API layers operate on the same objects as before.
- **SC-005**: Every tamper, wrong-key, mismatched-identity, and unknown-key-identifier case is detected and rejected on read — 0% return corrupted or plaintext data.
- **SC-006**: Misconfiguration (a configured key source that yields a missing/malformed/wrong-size key, or more than one key source configured) prevents startup 100% of the time, with no path that silently degrades to plaintext.
- **SC-007**: On the cipher path, encrypt or decrypt of a representative payload (≤ 8 KB) adds ≤ 1 ms at p95, verified by a microbenchmark; end-to-end request p95 latency with a key configured is within 5% of the no-key baseline. Full load/throughput testing is smoke-only.
- **SC-008**: With no key source configured, no marker is written and behavior and stored data are byte-for-byte identical to the pre-feature baseline; with a key source configured against an empty store, 100% of records are encrypted and the store carries an encryption marker. No configuration causes a deployment to mix encrypted and plaintext records.

## Assumptions

- **Opt-in via key-source presence, fixed per deployment**: storage encryption turns on when a key source is configured and is off otherwise (so development and existing behavior are unchanged when no key is set). There is no separate enable/disable flag — configuring a key is the opt-in. The choice is fixed for a populated store: a deployment does not mix encrypted and plaintext records.
- **Rollout via the Miden 0.15 cutover**: the production data store is emptied by the 0.15 account-ID cutover migration, so an operator can switch on encryption against an empty store and have every record encrypted from the first write. This avoids a mixed plaintext/ciphertext data set and a dual-read backfill; within an encryption-enabled deployment a plaintext payload on read is treated as an error, not a supported legacy path. Turning encryption on (or off) for an already-populated store is a separate, explicit re-encryption migration and is out of scope here.
- **Disk-level encryption remains in place**: managed-key volume encryption on the production database is already configured and is unchanged by this feature. Application-layer encryption is additive defence-in-depth, not a replacement.
- **Non-custodial scope**: Guardian holds no account spending keys at rest; the data being protected is account state/history confidentiality, not key material. This bounds the blast radius and informs the priority of this work.
- **Cipher and key size**: an industry-standard authenticated cipher with a 256-bit key is assumed; the exact algorithm is an implementation choice that does not affect the requirements above.
- **Production key store is AWS Secrets Manager**: chosen for consistency with the existing ACK-key path (access-controlled, auditable, rotatable, kept out of process configuration and logs). It delivers the raw key bytes into the Guardian process, so it does **not** provide a "key never resident in memory" guarantee — that property is the separate enclave/private-mode effort and is out of scope here. A future KMS-wrapped-DEK or enclave-backed provider can be added behind the same key-provider seam without changing the stored envelope format.
- **Rotation tooling**: the initial release records key identity to *enable* rotation; building active rotation/re-encryption tooling is a follow-up and is not required for the requirements here.
- **Key identity and secret shape**: each envelope's key id (`kid`) names the key that produced it. The production key secret is a structured document carrying one or more keys keyed by `kid` plus a designated active `kid`; the development key uses a configured `kid` (a fixed default if unset). This lets records written under a previous key still decrypt after the active key changes (FR-011), without storing the key in the envelope.
- **Keys are cached in-process**: keys are fetched once at startup and held in the zeroize-on-drop secret wrappers for the process lifetime. Decrypt/encrypt never calls the key store per operation; the key store (e.g., Secrets Manager) is a startup dependency, not a per-request one.

## Out of Scope

- **Private mode / enclave**: keeping plaintext out of the running process's memory (e.g., enclave-resident keys with attested release) is a separate, larger effort and is not addressed here. This feature's key is resident in the Guardian process in the base configuration.
- **Encrypting routing/index fields or supporting search over encrypted contents**: routing fields stay plaintext by design; querying *into* encrypted payloads is not a goal.
- **Active key rotation and bulk re-encryption tooling**: see Assumptions.
- **Changes to how signing/ACK keys are stored**: those already use a managed key path and are unaffected.
