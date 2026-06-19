# Feature Specification: Standards-Based Database TLS Certificate Verification

**Feature Branch**: `001-db-tls-verification`  
**Created**: 2026-06-04  
**Status**: Draft  
**Input**: User description: "Analyze and propose improvements for issue #168. It should follow standards and behave the way other users expect it to do. Have in mind not just AWS would be used — users could use some other provider or local docker compose."

## Context

Guardian's server can store state, deltas, and audit records in Postgres. When a
deployment points `DATABASE_URL` at a TLS-capable database (today: AWS RDS via
`sslmode=require`), the connection is **encrypted but not authenticated** — the
server currently accepts *any* certificate the database presents. This defeats
the purpose of TLS against an active network attacker: a man-in-the-middle can
present a forged certificate and the server will connect anyway, exposing
credentials and all custody-relevant data in transit.

The fix must not be AWS-specific. Operators run Guardian against AWS RDS, other
managed Postgres providers (GCP Cloud SQL, Azure, Supabase, Neon, etc.), and
self-hosted/local `docker compose` databases. The behavior therefore uses the
**standard Postgres `sslmode` / `sslrootcert` parameter names and their meanings**
that every Postgres client and operator already understands — so a `verify-full`
+ `sslrootcert` configuration means what it means in `psql`. Guardian applies a
deliberate, documented **fail-closed policy subset** on top of those standard
names rather than reproducing libpq's most permissive fallbacks: it rejects the
plaintext-fallback negotiation modes (`allow`/`prefer`), treats an absent
`sslmode` as `disable` (no implicit TLS negotiation), and does not honor libpq's
implicit default trust files. These deviations are intentional security policy
for a custody system and are called out explicitly (see Assumptions) so they are
not a surprise — the parameter vocabulary is standard; the policy is stricter.

The intended outcome: an operator can configure Guardian to *authenticate* the
database it connects to (verify the certificate chain, and optionally the
hostname) using a trust anchor they control, across any provider, while
existing encrypt-only and plaintext-local deployments keep working unchanged
unless the operator opts into stronger verification.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Authenticated TLS to a managed Postgres provider (Priority: P1)

An operator deploying Guardian against a managed Postgres service (AWS RDS or
any other provider) wants the server to *prove* it is talking to the real
database before sending credentials and custody data. They configure the
standard "verify the server" mode and point Guardian at the certificate
authority (CA) bundle for their provider. Connections to the genuine database
succeed; connections to an impostor presenting an untrusted or mismatched
certificate are refused.

**Why this priority**: This is the security defect in issue #168 and the core
value of the feature. Without it, TLS provides confidentiality but no
authentication, leaving custody data exposed to active MITM attacks. It is a
complete, demonstrable slice on its own.

**Independent Test**: Stand up a Postgres endpoint with a known CA. Configure
Guardian for full verification with that CA bundle and confirm startup + pool
creation succeed. Then point Guardian at an endpoint presenting an untrusted
or hostname-mismatched certificate and confirm the connection is refused with a
clear error.

**Acceptance Scenarios**:

1. **Given** a database whose certificate chains to a configured trusted CA and
   whose hostname matches the certificate, **When** Guardian starts in the
   verify-the-server mode, **Then** migrations run, the connection pool is
   created, and the server becomes ready.
2. **Given** a database presenting a certificate that does NOT chain to any
   configured trusted CA, **When** Guardian attempts to connect in a verifying
   mode, **Then** the connection is refused and the operator sees an error
   identifying certificate verification as the cause.
3. **Given** a database whose certificate is valid but whose hostname does not
   match the connection endpoint, **When** Guardian connects in full
   (chain + hostname) verification mode, **Then** the connection is refused.

---

### User Story 2 - Provider-agnostic trust configuration (Priority: P2)

An operator on a non-AWS provider (or self-hosting) wants to supply their own
trust anchor without Guardian assuming AWS. They can either point Guardian at an
explicit CA bundle file they mount into the environment, or instruct Guardian to
use the host's system trust store for providers whose certificates chain to
well-known public roots.

**Why this priority**: Directly addresses the "not just AWS" requirement. The
P1 mechanism must be expressed generically so that the same configuration model
serves every provider; this story guarantees no AWS-specific assumption leaks
into the contract.

**Independent Test**: With the P1 verifier in place, configure trust via (a) an
explicit CA bundle file path and (b) the system trust store, and confirm both
resolve and verify correctly against matching endpoints.

**Acceptance Scenarios**:

1. **Given** an operator sets the trust anchor to an explicit CA bundle file,
   **When** Guardian connects to a database whose chain validates against that
   file, **Then** the connection succeeds.
2. **Given** an operator points `sslrootcert` at a single combined CA bundle
   containing multiple roots (e.g. for a provider that fronts the database with a
   proxy using a different CA), **When** Guardian connects to either endpoint
   whose chain validates against some root in the bundle, **Then** the connection
   succeeds — no per-endpoint reconfiguration.
3. **Given** a verifying mode is selected but the configured CA bundle file is
   missing, unreadable, empty, or only partially parseable, **When** Guardian
   starts, **Then** startup fails fast with an error naming the misconfigured
   trust setting (no silent fallback to accept-any-certificate).
4. **Given** `sslrootcert=system` is set, **When** Guardian starts, **Then** it
   fails fast with an error stating the host trust store is not supported yet and
   an explicit CA bundle file is required (see FR-003a).

---

### User Story 3 - Local and encrypt-only deployments keep working (Priority: P3)

An operator running a local `docker compose` Postgres (no TLS) or an existing
encrypt-only deployment expects current behavior to continue without forced
changes. Plaintext local connections still work; an explicit encrypt-only mode
still encrypts without demanding a CA.

**Why this priority**: Protects the developer/local workflow and avoids breaking
deployments that have not yet provisioned a trust anchor. It bounds the blast
radius of the change and follows least-surprise.

**Independent Test**: Connect to a local plaintext Postgres with no TLS settings
and confirm normal operation. Separately, use the encrypt-only mode and confirm
the connection encrypts and succeeds without a CA bundle.

**Acceptance Scenarios**:

1. **Given** a connection string with no `sslmode` (omitted), **When** Guardian
   connects to a local docker compose Postgres, **Then** it connects in plaintext
   (absent is normalized to `disable` on BOTH the migration and pool paths), with
   no certificate requirements.
2. **Given** `sslmode=disable`, **When** Guardian connects, **Then** it connects
   in plaintext on both paths.
3. **Given** `sslmode=require` with no `sslrootcert`, **When** Guardian connects
   to a TLS-capable server, **Then** the channel is encrypted and no certificate
   verification is performed, on both paths.
4. **Given** `sslmode=require` with no `sslrootcert`, **When** the target server
   does NOT support TLS, **Then** the connection is refused (no silent plaintext
   downgrade), on both paths.
5. **Given** each of scenarios 1–4, **When** exercised, **Then** the synchronous
   migration path and the asynchronous pool path exhibit the SAME behavior
   (validated explicitly, per FR-007 / Constitution Principle V).

### Edge Cases

- **Expired or not-yet-valid certificate**: in a verifying mode the connection
  MUST be refused, even if the chain is otherwise trusted.
- **CA rotation**: an operator can update the trust anchor (new/added CA) and
  restart without code changes; a bundle containing multiple CAs is supported so
  old and new roots can coexist during rotation.
- **Verifying mode requested but no trust anchor resolvable**: startup MUST fail
  closed with a clear error rather than connect insecurely.
- **TLS requested but the server does not support TLS**: surfaced as a clear
  connection error, not a silent plaintext downgrade.
- **Two TLS stacks reconciled**: the startup-migration connection and the
  runtime connection pools use different TLS engines today. A given
  configuration (e.g. a single CA bundle file) MUST verify on both. A
  regression where migrations connect insecurely while pools verify (or vice
  versa) MUST be treated as a failure of this feature.
- **Self-signed certificate for a local TLS test**: works only when the
  operator supplies that certificate as the trust anchor. At `verify-ca` the
  certificate's hostname need not match the endpoint; at `verify-full` a
  self-signed certificate MUST additionally carry a subject matching the
  connection host, otherwise it is refused. Local TLS acceptance tests should
  cover `verify-ca` (chain only) and `verify-full` (chain + hostname)
  distinctly.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST support the standard Postgres TLS verification
  ladder and behave consistently with how widely-used Postgres clients treat
  each level: a plaintext/no-TLS level, an encrypt-only level (encrypt without
  certificate verification), a chain-verification level (verify the certificate
  chains to a trusted CA), and a full-verification level (verify the chain AND
  that the server hostname matches the certificate).
- **FR-001a**: The verification level MUST be derived by parsing the standard
  `sslmode` token in the database connection string (the established
  source-of-truth and least-surprise contract). It MUST NOT use a substring
  check; in particular the current `contains("sslmode=require")` behavior — which
  silently routes `verify-ca`/`verify-full` to the plaintext branch — MUST be
  replaced by full token parsing. An unrecognized `sslmode` value MUST fail fast
  rather than degrade to a weaker level.
- **FR-001b**: The system MUST explicitly define which `sslmode` values it
  *supports* versus merely *recognizes*, and MUST NOT silently treat an
  unsupported value as a weaker one:
  - **Supported (enforced identically on both stacks):** `disable` (no TLS),
    `require` (encrypt-only, subject to FR-001c), `verify-ca` (chain),
    `verify-full` (chain + hostname).
  - **`allow` / `prefer`:** these standard modes negotiate and *fall back to
    plaintext* (allow = plaintext-first, prefer = TLS-first-with-plaintext-
    fallback). Silent plaintext fallback is unacceptable for a custody system
    (conflicts with FR-004's fail-closed posture), and the async driver does not
    natively model the full ladder (it exposes only disable/prefer/require).
    **Decision (resolved in planning):** Guardian REJECTS `allow`/`prefer` at
    startup with an actionable error pointing the operator to an explicit mode
    (`disable`/`require`/`verify-ca`/`verify-full`). It MUST NOT accept them
    while quietly falling back to plaintext.
- **FR-001c**: The system MUST resolve the well-known libpq compatibility rule
  that `sslmode=require` is *promoted to chain verification* when a root CA
  (`sslrootcert`) is supplied. Because the synchronous libpq stack applies this
  promotion automatically while the async stack would not unless taught to, the
  system applies ONE normalized behavior identically to both stacks.
  **Decision (resolved in planning):** follow libpq — `require` + a configured
  `sslrootcert` ⇒ behave as `verify-ca` (chain verification). `require` with no
  `sslrootcert` remains encrypt-only. The async stack MUST replicate this
  promotion so the two stacks never diverge (FR-007). The rule MUST be
  documented.
- **FR-002**: When a chain-verification or full-verification level is selected,
  the system MUST reject any connection whose server certificate does not
  validate against the configured trust anchor, including untrusted issuers,
  expired/not-yet-valid certificates, and (at the full level) hostname
  mismatches.
- **FR-002a**: The canonical hostname-verification semantics for `verify-full`
  are **strict, SAN-based (RFC 6125-style)** matching as implemented by the async
  stack's TLS library — NOT libpq's more lenient legacy behavior (Common-Name
  fallback, and its particular IP-address handling). Server certificates MUST
  carry the connection hostname (or IP) in a Subject Alternative Name;
  Common-Name-only certificates are NOT supported. This is the stricter of the
  two stacks, so anything the async stack accepts libpq also accepts (no
  divergence in the accepting direction); the spec adopts strict-SAN as the
  single contract. Managed providers (AWS RDS, RDS Proxy/ACM, GCP, Azure) issue
  SAN-based certificates, so this is satisfied in practice. Cross-stack tests MUST
  cover: DNS-name SAN match, IP-address SAN match, SAN mismatch (refused), and a
  CN-only certificate (refused).
- **FR-003**: The system MUST allow the trust anchor to be configured by the
  operator independent of any specific cloud provider. The supported mechanism
  for this feature is **(a) an explicit CA bundle file** provided by the operator
  (works for AWS RDS and any provider whose CA is mounted). **(b) the host system
  trust store** (`sslrootcert=system`) is recognized but rejected for now per
  FR-003a (deferred pending a libpq runtime upgrade).
  The configuration MUST follow standard Postgres `sslrootcert` semantics —
  `sslrootcert=<path>` for an explicit CA bundle and `sslrootcert=system` for
  the host trust store — so the knob matches operator expectations and applies
  uniformly across the connection consumers in FR-007.
- **FR-003a**: The `sslrootcert=system` (host trust store) option carries two
  standards-driven constraints the system MUST honor:
  - Per libpq semantics, `sslrootcert=system` forces `verify-full` (weaker modes
    are rejected when system roots are selected). The system MUST enforce this
    rather than allow `sslrootcert=system` with `verify-ca` or weaker.
  - `sslrootcert=system` was introduced in PostgreSQL 16. The runtime image's
    libpq (currently Debian Bookworm's `libpq5`, which is PG15) does not support
    it, so the synchronous migration stack cannot honor `sslrootcert=system`.
    **Decision (resolved in planning):** `sslrootcert=system` is scoped OUT of
    this feature — Guardian rejects it at startup with an error explaining that
    an explicit CA bundle file (`sslrootcert=<path>`) is required until the
    runtime libpq is upgraded to ≥16. The explicit CA bundle file is fully
    supported on both stacks now and is the portable standard option. (Adding
    system-store support via a libpq upgrade is a documented future follow-up.)
- **FR-004**: Accept-any-certificate behavior MUST be **unreachable from any
  verifying mode** (`verify-ca`, `verify-full`). A non-verifying TLS verifier
  remains explicitly permitted for exactly one case — encrypt-only (`require`
  without `sslrootcert`), which is by definition not a verifying mode and matches
  standard `require` semantics. There MUST be no configuration, default, or code
  path by which a verifying mode resolves to acceptance of an unvalidated
  certificate. (This supersedes the earlier "remove entirely" wording: the
  capability is retained solely for encrypt-only and is unreachable elsewhere.)
- **FR-005**: When a verifying mode is requested but the required trust anchor is
  missing, unreadable, empty, **or only partially parseable**, the system MUST
  fail fast at startup with an actionable error identifying the misconfigured
  setting, and MUST NOT silently fall back to a non-verifying connection. The
  ENTIRE CA bundle MUST parse successfully — a bundle with any malformed PEM entry
  is rejected (no silently-dropped certificates that could leave a partial trust
  store), and this check happens before migrations run (see FR-007a).
- **FR-005a**: All connection and TLS error messages and logs MUST keep database
  credentials and sensitive connection parameters redacted — the database
  password, and any secret query parameters, MUST NOT appear in errors, logs, or
  panics surfaced by the new code paths. This MUST be covered by an acceptance
  test asserting that a failed verifying connection produces a safe, redacted
  message. (Consistent with the existing `CredentialUrl` secret-wrapping
  treatment of `DATABASE_URL`.)
- **FR-006**: The system MUST preserve existing behavior for the no-TLS level
  (local/plaintext) and the encrypt-only level so current local docker compose
  and encrypt-only deployments continue to start and operate without new
  required configuration.
- **FR-007**: All database connection consumers (state storage, metadata store,
  audit store, and startup migrations) MUST apply the same TLS verification
  behavior **for a given `DATABASE_URL`**, with no consumer connecting more
  permissively than another. This explicitly spans **two distinct TLS stacks**
  used today: the synchronous startup-migration path and the asynchronous
  connection-pool path resolve and enforce TLS through different mechanisms (see
  Assumptions). Parity is scoped to the `DATABASE_URL` connection-string inputs
  within Guardian's controlled runtime image; libpq's *implicit* trust sources
  (`PGSSL*` environment variables, `~/.postgresql/root.crt`, `…/root.crl`) are NOT
  a supported configuration channel and MUST be neutralized per FR-007a so they
  cannot make the migration stack diverge from the pool stack.
- **FR-007a**: Cross-stack parity MUST be enforced by a single Guardian-level
  **preflight that runs before `run_migrations`** (the production startup runs
  migrations before pool creation), not by hoping libpq and rustls independently
  agree. The preflight parses and validates `(sslmode, sslrootcert)` once and
  rejects `allow`/`prefer`/`system`/unknown and verify-without-`sslrootcert`
  before any database connection is attempted. To keep the more-permissive libpq
  stack from diverging via implicit sources, Guardian MUST: (i) for verifying
  modes, pass an **explicit `sslrootcert`** to libpq so it never falls back to
  `~/.postgresql/root.crt`; (ii) normalize absent `sslmode` to `disable` for the
  migration URL (libpq's default is `prefer`); and (iii) rely on the runtime
  image carrying no conflicting `PGSSL*`/dotfile defaults (verified: none today).
  Unifying both consumers onto a single TLS stack was considered but rejected —
  diesel migrations require a synchronous libpq connection and there is no
  rustls-backed async migration harness; the preflight + explicit-params +
  controlled-image approach achieves equivalent guarantees.
- **FR-008**: TLS verification configuration MUST be expressible through the
  deployment's standard configuration surface (connection settings and/or
  environment), and MUST be documented for AWS RDS, at least one other managed
  provider, and local docker compose so operators can reproduce the expected
  behavior.
- **FR-009**: The AWS reference deployment MUST be updated to request an
  authenticating verification level (`verify-full`) and to supply the appropriate
  trust anchor, so the default managed deployment is authenticated rather than
  encrypt-only, while keeping ECS → RDS startup and pool creation working.
- **FR-009a (trust anchor — combined bundle)**: Production defaults to the **RDS
  Proxy** endpoint, whose certificate is issued via AWS Certificate Manager and
  chains to **Amazon Trust Services** roots — NOT the Amazon RDS CA roots used by
  a direct instance endpoint. Therefore the AWS trust anchor MUST be a **combined
  CA bundle** containing both the Amazon RDS CA roots AND the Amazon Trust
  Services roots, so `verify-full` succeeds whether `DATABASE_URL` targets the
  proxy endpoint or a direct instance. A single-source RDS-only bundle would fail
  the handshake against the proxy and is incorrect.
- **FR-009b (delivery mechanism — resolved)**: The published Guardian server image
  MUST remain **provider-neutral and carry no baked-in CA bundle** (the image runs
  on non-AWS infra too). The combined CA bundle is **provided to the container at
  deploy time by the infrastructure** (mounted file / deploy-tooling-placed), at a
  documented fixed container path, with Terraform setting `sslrootcert` to that
  path; the application never downloads it. Rotation is a defined procedure
  (replace the mounted multi-root bundle and restart/redeploy), requires no code
  or image change, and MUST place the new trust anchor before any mode tightening
  (relates to FR-001c and the sequencing assumption).
- **FR-010**: Operator-facing documentation MUST explain how to choose a
  verification level and configure a trust anchor per provider, including a
  CA-rotation procedure and a troubleshooting entry mapping verification
  failures to causes.

### Key Entities *(include if feature involves data)*

- **Verification level**: the operator's chosen strictness for a database
  connection (no-TLS, encrypt-only, verify-chain, verify-full), mirroring
  standard Postgres `sslmode` semantics.
- **Trust anchor**: the set of certificate authorities the system trusts when
  verifying a server certificate — an operator-provided CA bundle file (possibly
  combining multiple roots, e.g. RDS + Amazon Trust Services for proxy + direct).
  The host system trust store is out of scope this feature (FR-003a).
- **Database connection target**: the endpoint (host/port) and requested
  verification level that each connection consumer uses; the hostname is the
  subject of full verification.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: In any verifying mode (`verify-ca` or `verify-full`), 100% of
  connection attempts to a server presenting an untrusted or expired certificate
  are refused. Additionally, in `verify-full`, 100% of connections to a server
  whose hostname does not match its certificate are refused. (Hostname mismatch
  is NOT a failure condition under `verify-ca`, which by definition does not
  check hostname — see FR-002.) Net: zero successful connections that the
  selected mode is supposed to reject.
- **SC-002**: With a correctly configured trust anchor, a Guardian deployment
  against a managed Postgres provider starts, runs migrations, and creates its
  connection pool successfully on the first attempt.
- **SC-003**: The same documented configuration model verifies connections
  successfully against at least three deployment targets — AWS RDS, one other
  managed provider, and a local TLS-enabled Postgres — with no provider-specific
  code branches. The local TLS-enabled target MUST be covered by an automated
  test; the two managed-provider legs (which require live cloud credentials) are
  verified via documented **manual smoke** procedures per AGENTS.md §6, not as
  an automated CI gate.
- **SC-004**: Existing local docker compose (plaintext) and encrypt-only
  deployments require zero new configuration to keep working after the change.
- **SC-005**: When verification is misconfigured (missing/invalid trust anchor
  in a verifying mode), the operator can identify and resolve the cause from the
  startup error and documentation in under 10 minutes, without reading source
  code.
- **SC-006**: A security reviewer can confirm by inspection that no code path
  remains which accepts an unverified certificate in a verifying mode.

## Assumptions

- The standard Postgres `sslmode` ladder is adopted as the behavioral contract,
  with these precise semantics (not the earlier simplification):
  `disable` = no TLS; `allow` = plaintext-first, TLS only if the server demands
  it; `prefer` = TLS-first with plaintext fallback; `require` = encrypt-only
  **unless** a root CA is configured, in which case libpq promotes it to chain
  verification (see FR-001c); `verify-ca` = verify chain; `verify-full` = verify
  chain + hostname. The defect being fixed is the absence of any verifying mode
  and the substring-based detection — NOT that `require` is wrong by itself.
  Terraform's `sslmode=require` was used where authentication was intended; with
  FR-001c, supplying the RDS CA turns that same `require` into chain
  verification, which is the least-surprise migration path. The negotiation
  fallback modes (`allow`/`prefer`) are handled per FR-001b rather than treated
  as plain "no-verify".
- **Two TLS stacks exist today and must be reconciled (feeds FR-007).** Startup
  migrations connect through the synchronous libpq-backed driver, which reads
  `sslmode`/`sslrootcert` from the connection string natively. The runtime
  state/metadata/audit pools connect through the asynchronous rustls-backed
  driver, where the trust anchor must be loaded into an in-memory root store and
  the certificate verifier explicitly chosen — the path that currently hard-codes
  accept-any. The production startup runs both back-to-back. Planning MUST budget
  for making one operator configuration drive *both* engines identically, rather
  than discovering the split mid-implementation.
- The trust-anchor delivery to the running container (mounting the CA bundle file
  at the `sslrootcert` path) is an infrastructure/deployment task, not application
  logic, performed **at deploy time** (FR-009b). The published image stays
  provider-neutral with no baked CA. For AWS the reference infra mounts the
  **combined** bundle (RDS + Amazon Trust Services roots, FR-009a) and sets
  `sslrootcert`. This must be in place before the AWS reference mode is tightened
  (FR-009).
- The fix stays provider-neutral: no cloud-provider-specific certificate is
  embedded in the application binary OR the published image. AWS RDS is supported
  through the same generic trust-anchor mechanism (the reference infra mounts a
  combined CA bundle at deploy time), not through AWS-only code. *(Alternative
  considered and rejected: baking a vendor CA bundle into the image for
  zero-config convenience — rejected to honor the "not just AWS" requirement and
  keep the published image reusable on any infra; the operator/infra mounts the
  bundle their provider needs.)*
- "Full" verification (chain + hostname) is the recommended target for managed
  providers because their endpoints match their certificate subjects; the
  reference AWS deployment will adopt it.
- Changing the AWS reference deployment's verification level requires the trust
  anchor to be present before the stricter mode is enabled, to satisfy the
  issue's "keep startup and pool creation working" constraint; sequencing is a
  deployment/migration concern to be detailed in planning.
- Local/self-hosted TLS testing uses operator-supplied certificates; Guardian
  does not generate or distribute certificates.

## Out of Scope

- Mutual TLS / client-certificate authentication to the database.
- Automatic download or bundling of any provider's CA certificates by the
  application or the published image.
- Host system trust store (`sslrootcert=system`) — deferred pending a libpq ≥16
  runtime upgrade (FR-003a).
- The negotiation/fallback modes `allow` and `prefer` (rejected, FR-001b).
- Certificate lifecycle automation (issuance, rotation scheduling, monitoring).
- TLS for any non-Postgres connection.
