# Feature Specification: Modular Hosted ECDSA Signer Backends

**Feature Branch**: `001-hosted-signer-backends`
**Created**: 2026-06-03
**Status**: Draft
**Input**: User description: "Start work on issue #227. Focus only on ECDSA, not Falcon at the moment. It should be modular — start with supporting just AWS KMS, but users should be able to contribute and add support for other hosted providers such as Turnkey, GCP KMS, HashiCorp Vault, and similar."

## Overview

The Guardian server acknowledges (acks) deltas by signing them with a server-held
ECDSA signing key. Today that key lives **inside the server process** as raw key
bytes, loaded either from a local file (dev) or from AWS Secrets Manager at boot
(production). Anything that can read process memory — a coredump, a debugger, an
errant debug log, a compromised dependency — can exfiltrate the key permanently,
forcing downstream cosigners and consumers to rotate trust.

This feature introduces a **modular hosted signer backend** capability for the
**ECDSA ack signer**. With a hosted backend selected, the server holds only a
*key handle* (e.g. an AWS KMS key identifier); the private key never enters the
server process. Signing happens behind a trust boundary (a remote signing call),
and a server compromise lets an attacker *use* the key while the compromise is
active but never *extract* it.

Scope is deliberately narrow and extensible:

- **ECDSA only.** The Falcon ack signer is out of scope and continues to operate
  exactly as it does today.
- **In-memory remains the default.** Hosted backends are opt-in. Existing
  file-based and Secrets-Manager deployments keep working with no configuration
  change.
- **Modular by design.** The first shipped hosted backend is **AWS KMS**. The
  abstraction must let contributors add additional backends (GCP KMS, HashiCorp
  Vault Transit, Turnkey, PKCS#11 HSM, custom HTTP signing service) without
  modifying the core ack/signing flow.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Operator runs the ack signer with AWS KMS (Priority: P1)

A production operator wants the server's ECDSA ack key to live in AWS KMS so that
a server compromise cannot exfiltrate it. They provision a secp256k1 KMS key,
grant the server's role permission to sign with it, and point the server at the
key by configuration. The server starts up, confirms it can reach the key and
read the public key, and from then on signs every ECDSA ack by calling KMS.

**Why this priority**: This is the core value of the issue — removing the signing
key from the process blast radius for production. Without it the feature delivers
nothing. It is the MVP.

**Independent Test**: Configure the server to use the AWS KMS ECDSA backend
against a KMS key (or a KMS-compatible local emulator), submit a delta for ack,
and verify the returned ack signature verifies against the published server
public key on the same secp256k1 scheme — identically to an in-memory-signed ack.

**Acceptance Scenarios**:

1. **Given** the server is configured to use the AWS KMS ECDSA backend with a
   valid key handle and signing permission, **When** the server starts,
   **Then** it loads the public key from KMS, derives and exposes the same
   public-key / commitment values the rest of the system already consumes, and
   reports ready.
2. **Given** a running server backed by AWS KMS, **When** a delta is submitted
   for ECDSA ack, **Then** the server requests a signature from KMS and returns
   an ack whose signature verifies against the server's published ECDSA public
   key, byte-for-byte interchangeable with an in-memory-produced ack.
3. **Given** the server is configured for AWS KMS but the key handle is missing,
   the curve is unsupported, or the role lacks sign permission, **When** the
   server starts, **Then** it fails fast with an actionable error naming the
   offending configuration and does not start in a half-signing state.
4. **Given** a transient KMS error during a single signing call, **When** an ack
   is requested, **Then** the request fails with a clear error and the server
   remains healthy for subsequent requests (no silent unsigned ack, no crash).

---

### User Story 2 - Existing deployments keep working unchanged (Priority: P1)

An operator on the current file-based (dev) or AWS Secrets Manager (production)
key path upgrades to the version containing this feature **without** changing any
signer configuration. Their ECDSA ack signer continues to load and sign exactly
as before.

**Why this priority**: The issue mandates that the in-memory path "stays as the
default." A regression here breaks every existing deployment, so it ships
alongside P1.

**Independent Test**: With no hosted-backend configuration present, start the
server using the existing file keystore and confirm ECDSA acks are produced with
the same public key, commitment, and signature format as before this change.

**Acceptance Scenarios**:

1. **Given** no hosted signer backend is configured, **When** the server starts,
   **Then** it uses the in-memory ECDSA signer (file or Secrets Manager) exactly
   as it does today, with no new required configuration.
2. **Given** an existing deployment's configuration, **When** it is run on the
   new version, **Then** the ECDSA public key, commitment, and ack signature
   output are unchanged.

---

### User Story 3 - Contributor adds a new hosted backend (Priority: P2)

A contributor wants to add support for a different hosted provider (e.g. GCP KMS,
HashiCorp Vault Transit, Turnkey, or a custom HTTP signing service). They
implement the documented signer backend contract for the new provider and
register it, without altering the ack flow, the in-memory path, or the existing
AWS KMS backend.

**Why this priority**: Modularity and community extensibility is an explicit goal
of the request ("users should be able to contribute and add support for other
hosted providers"). It is essential to the design but not required for the first
shippable slice (AWS KMS).

**Independent Test**: Add a second backend implementation (a stub or test
provider) behind the same contract, select it by configuration, and confirm acks
route through it with no edits to the core ack/signing code or to the AWS KMS
backend.

**Acceptance Scenarios**:

1. **Given** the documented signer backend contract, **When** a contributor adds
   a new provider implementing it and registers it under a backend identifier,
   **Then** selecting that identifier by configuration routes ECDSA acks through
   the new backend with no changes to unrelated code.
2. **Given** a configuration naming an unknown or unregistered backend
   identifier, **When** the server starts, **Then** it fails fast listing the
   identifiers it does support.

---

### Edge Cases

- **Identity continuity on migration**: An operator moving from an in-memory key
  to a hosted backend is moving to a *different* keypair (the KMS-held key),
  hence a different public key and commitment. The system MUST surface the
  resulting server public key / commitment so operators can re-establish
  downstream trust; it MUST NOT silently present a key it cannot actually sign
  with. (Importing an existing private key into a hosted backend is out of band
  and out of scope — see Assumptions.)
- **Signature-format compatibility**: Hosted backends commonly return signatures
  in encodings (e.g. DER, non-normalized high-S) that differ from the byte layout
  the Miden verifier and consumers expect. The produced ack MUST be normalized to
  the exact format an in-memory ECDSA signature uses, or the ack is invalid.
- **Curve / scheme mismatch**: A configured hosted key on the wrong curve or
  signing scheme (not the secp256k1-based scheme the ECDSA ack uses) MUST be
  rejected at startup, not at first sign.
- **Backend unreachable at boot vs. at runtime**: Unreachable at boot → fail
  fast. Transient failure at runtime → fail that ack with a clear error, stay
  healthy.
- **Latency and rate limits**: A hosted signing call adds network round-trips
  and may be rate-limited by the provider; an ack that exceeds the provider's
  limits MUST fail loudly rather than hang indefinitely.
- **Falcon coexistence**: When the ECDSA signer uses a hosted backend, the Falcon
  ack signer MUST continue to operate via its existing in-memory path within the
  same server.
- **Credential sourcing**: Hosted backends authenticate via the provider's own
  credential mechanism (e.g. the host's cloud role). Misconfigured or absent
  credentials MUST produce an actionable startup error.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST provide a backend-agnostic ECDSA signing
  abstraction such that the ack flow signs without knowing whether the key is
  in-memory or held by a hosted backend.
- **FR-002**: The in-memory ECDSA signer (file keystore and AWS Secrets Manager)
  MUST remain the default and MUST require no new configuration to continue
  operating as it does today.
- **FR-003**: Selecting a hosted ECDSA signer backend MUST be opt-in via
  explicit operator configuration.
- **FR-004**: The system MUST ship an **AWS KMS** hosted backend for the ECDSA
  ack signer as the first supported hosted provider.
- **FR-005**: When a hosted backend is configured, the server MUST hold only a
  key handle/identifier and MUST NOT load or retain the private key bytes in
  process memory.
- **FR-006**: An ECDSA ack produced by any hosted backend MUST be verifiable
  against the server's published ECDSA public key on the same scheme as an
  in-memory-produced ack, and MUST be byte-compatible with the signature format
  consumers and the Miden verifier already accept (including any required
  normalization such as low-S / fixed encoding).
- **FR-007**: At startup with a hosted backend configured, the system MUST
  validate reachability, key existence, curve/scheme compatibility, and signing
  permission, and MUST fail fast with an actionable error if any check fails.
- **FR-008**: The system MUST expose the server's ECDSA public key and commitment
  for whichever backend is active, so downstream trust can be established the
  same way regardless of backend.
- **FR-009**: A runtime signing failure from a hosted backend MUST surface as a
  clear, typed error for that ack request and MUST NOT crash the server or emit
  an unsigned/placeholder ack.
- **FR-010**: The backend abstraction MUST be extensible so a contributor can add
  a new provider (GCP KMS, HashiCorp Vault Transit, Turnkey, PKCS#11 HSM, custom
  HTTP signer) by implementing the documented contract and registering it,
  without modifying the ack flow, the in-memory path, or other backends.
- **FR-011**: The active backend MUST be selectable by a backend identifier in
  configuration; an unknown identifier MUST cause a fail-fast startup error that
  lists the supported identifiers.
- **FR-012**: The Falcon ack signer MUST be unaffected; it continues to use its
  existing in-memory path even when the ECDSA signer uses a hosted backend.
- **FR-013**: Hosted backends MUST NOT log or otherwise expose key material; the
  only signing-key-derived values that may be surfaced are the public key and
  commitment.
- **FR-014**: Operator-facing documentation MUST describe how to configure the
  AWS KMS backend (required permissions, key requirements, configuration keys)
  and how a contributor adds a new backend.

### Key Entities *(include if feature involves data)*

- **ECDSA Signer Backend**: The abstraction the ack flow signs through. Knows how
  to produce an ECDSA signature over a given message and to report the public key
  and commitment. Has a stable backend identifier. Implementations: in-memory
  (default), AWS KMS (first hosted), and contributor-added providers.
- **Key Handle**: An opaque, provider-specific reference to a hosted key (e.g. an
  AWS KMS key identifier). Held by the server in place of private key bytes when
  a hosted backend is active.
- **Ack Signature**: The ECDSA signature attached to an acked delta. Its format
  and verification semantics are identical regardless of the backend that
  produced it.
- **Backend Selection Configuration**: Operator-provided settings naming which
  backend is active and supplying that backend's parameters (key handle, region,
  endpoint, etc.).

## Assumptions

- **Sign-only, existing key**: Hosted backends sign with a key the operator has
  already provisioned in the provider. Key *creation*, *import*, and *rotation*
  are performed out of band using the provider's own tooling and are out of
  scope for this feature.
- **Curve/scheme**: The ECDSA ack uses the secp256k1-based scheme already in the
  codebase. Hosted backends must support signing on that scheme; AWS KMS does via
  its secp256k1 key type. Providers that cannot sign on this scheme are not
  candidates for this feature.
- **Identity change is expected on migration**: Moving an existing deployment
  from an in-memory key to a hosted backend changes the server's ECDSA keypair
  and therefore its public key/commitment. Re-establishing downstream trust after
  such a migration is an operator action, not an automated migration.
- **Credentials via provider-native mechanisms**: The server authenticates to a
  hosted backend using that provider's standard credential chain (e.g. the host's
  cloud IAM role), consistent with how the existing Secrets Manager path
  authenticates.
- **No protocol or wire-contract change**: This feature changes only *where* the
  ECDSA signature is produced. The delta/ack wire contract, the verifier, and the
  cosigner/consumer trust model are unchanged.
- **No client SDK change**: Backend selection is server-side only; Rust and
  TypeScript multisig clients are unaffected.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** *(design guarantee, reviewer-asserted)*: With a hosted backend
  active, the server never receives the ECDSA private key — the backend type holds
  only a key handle and exposes no path to private key bytes (KMS does not return
  them). Verified by code review and by the type containing no secret-key field,
  rather than by runtime memory inspection.
- **SC-002**: 100% of ECDSA acks produced via the AWS KMS backend verify against
  the server's published ECDSA public key and are accepted by the same verifier
  that accepts in-memory-produced acks, with no protocol change.
- **SC-003**: Existing file-based and Secrets-Manager deployments upgrade to this
  version with **no** signer configuration change and observe identical public
  key, commitment, and ack signature behavior (zero regressions).
- **SC-004**: A misconfigured hosted backend (missing key, wrong curve, missing
  permission, unknown backend identifier, unreachable endpoint) is detected at
  **startup** in 100% of cases, with an error message that names the specific
  cause — never a silent or first-request failure.
- **SC-005**: A contributor can add a new hosted backend and route acks through
  it by editing **only** the new backend's implementation/registration — zero
  edits to the ack flow, the in-memory backend, or the AWS KMS backend (verified
  by a reviewer or a stub-backend test).
- **SC-006**: When the ECDSA signer uses a hosted backend, Falcon acks continue
  to be produced correctly in the same server (no Falcon regression).
