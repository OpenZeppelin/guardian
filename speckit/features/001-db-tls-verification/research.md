# Phase 0 Research: Database TLS Certificate Verification

All findings are version-accurate against `Cargo.lock`: `rustls 0.23.37`,
`tokio-postgres 0.7.17`, `tokio-postgres-rustls 0.12.0`, `diesel 2.2.12`,
`diesel-async 0.5.2`, `url 2.5`. Runtime image: `debian:bookworm-slim` + `libpq5`
(PostgreSQL 15) for the server binary (`Dockerfile:54-59`).

---

## D1. Source of truth for verification level + trust anchor

- **Decision**: Read standard libpq `sslmode` and `sslrootcert` from
  `DATABASE_URL`. No new environment variable is introduced.
- **Rationale**: The sync migration path (`PgConnection::establish`, libpq) already
  honors these natively; using the same parameters for the async path gives a
  single source of truth and satisfies operator expectations ("behave like
  psql"). Avoids a parallel Guardian-specific config surface that could drift
  between stacks (FR-007).
- **Alternatives considered**: A dedicated `GUARDIAN_DB_TLS_*` env var set —
  rejected: duplicates libpq semantics, invites divergence, and is non-standard.

## D2. tokio-postgres rejects unknown parameters; SslMode is Disable/Prefer/Require only

- **Finding**: `tokio_postgres::Config` parsing errors on unknown keys (e.g.
  `sslrootcert`) and accepts only `sslmode` ∈ {`disable`,`prefer`,`require`}
  (`tokio-postgres-0.7.17/src/config.rs`: unknown-key error path ~L715-719;
  `SslMode` enum ~L44-53; sslmode parse ~L584-591).
- **Consequence**: Guardian MUST parse and **strip** `sslrootcert` and any
  `verify-ca`/`verify-full` token from the URL before calling
  `tokio_postgres::connect`, and set `sslmode=require` to force the TLS handshake
  through our connector. The real verification level is enforced by the rustls
  verifier we install, not by tokio-postgres.
- **Decision**: Use the `url` crate (already a dependency) to parse query pairs,
  extract `sslmode`/`sslrootcert`, and rebuild a sanitized connection string for
  the async stack. The sync stack receives the original URL unchanged.

## D3. verify-full hostname checking is free from rustls' default verifier

- **Finding**: `tokio_postgres_rustls::MakeRustlsConnect::make_tls_connect(host)`
  converts the connection host into a `ServerName` and hands it to rustls
  (`tokio-postgres-rustls-0.12.0/src/lib.rs` ~L43-49). rustls' default
  `WebPkiServerVerifier` (from `ClientConfig::builder().with_root_certificates(..)`)
  performs chain validation, validity-period checks, AND hostname/SAN matching.
- **Decision**: For `verify-full`, build the standard
  `ClientConfig::builder().with_root_certificates(root_store).with_no_client_auth()`
  — do NOT call `.dangerous()`. Hostname verification comes for free.

## D4. verify-ca (chain only, no hostname) verifier

- **Decision**: Implement a small custom `ServerCertVerifier` that delegates chain
  + signature validation to a `WebPkiServerVerifier` built from the same root
  store, but does NOT fail on hostname mismatch. `verify_tls12_signature` /
  `verify_tls13_signature` / `supported_verify_schemes` delegate to the inner
  webpki verifier.
- **Rationale**: rustls 0.23 has no built-in "skip hostname" switch; a thin
  delegating verifier is the standard approach and keeps real chain validation
  intact (unlike the removed `NoCertificateVerification`).
- **Implementation note (verify at build time)**: the precise way to suppress the
  hostname check — either constructing the inner verifier and ignoring the
  specific `CertificateError` hostname variant, or calling rustls' lower-level
  `verify_server_cert_signed_by_trust_anchor` + signature helpers directly — must
  be confirmed against the exact `rustls 0.23.37` API during implementation. Both
  are viable; the delegating-and-filtering form is simplest. This is the one spot
  where the exact enum/API name should be checked, not assumed.

## D5. Loading the CA bundle

- **Finding**: `rustls-pemfile` is NOT yet a direct dependency. `rustls-native-certs
  0.8.3` and `webpki-roots 1.0.6` ARE already in the lockfile (transitive via the
  `aws-config`/`aws-sdk` rustls features) but are only needed for the system trust
  store, which is out of scope this feature.
- **Decision**: Add `rustls-pemfile = "2"` under the `postgres` feature. Load the
  operator's CA bundle file (the `sslrootcert` path) into a `rustls::RootCertStore`
  via `rustls_pemfile::certs(&mut reader)`. A bundle with multiple PEM certs is
  supported (CA rotation overlap).
- **Errors**: missing/unreadable/empty bundle in a verifying mode → fail fast with
  an actionable, credential-redacted error (FR-005/FR-005a).
- **PEM parsing note**: `rustls_pemfile::certs(&mut reader)` (v2) returns an
  iterator of `Result<CertificateDer, _>`. The loader MUST collect/propagate parse
  errors (e.g. `.collect::<Result<Vec<_>,_>>()?`), NOT `.filter_map(Result::ok)` —
  silently dropping malformed certs could leave an empty/partial root store and a
  misleading failure. An empty result in a verifying mode is itself a hard error.

## D6. `require` + `sslrootcert` promotion (libpq compatibility)

- **Finding**: libpq promotes `sslmode=require` to `verify-ca` behavior when a
  root CA file is configured. The sync stack does this automatically; the async
  stack would not unless taught to.
- **Decision (resolved)**: Follow libpq. In the async resolver, `require` +
  present `sslrootcert` ⇒ `verify-ca`; `require` without `sslrootcert` ⇒
  encrypt-only. This keeps both stacks identical (FR-001c, FR-007) and makes the
  existing Terraform `sslmode=require` upgrade to chain verification simply by
  supplying the RDS CA — a clean migration.

## D7. `allow` / `prefer` handling

- **Finding**: `allow` (plaintext-first) and `prefer` (TLS-first with plaintext
  fallback) can silently end up on plaintext; tokio-postgres cannot even model
  `allow`. Silent fallback contradicts the security goal and Constitution
  Principle III.
- **Decision (resolved)**: **Reject** `allow`/`prefer` at pool/connection setup
  with an actionable error pointing to `disable`/`require`/`verify-ca`/
  `verify-full`. No current deployment uses them; reversible later.

## D8. `sslrootcert=system` scope

- **Finding**: `sslrootcert=system` was added in PostgreSQL 16 and additionally
  forces `verify-full`. The server runtime image's `libpq5` is PG15, so the sync
  migration stack cannot honor it; supporting it only on the async stack would
  violate FR-007 (stack divergence).
- **Decision (resolved)**: Scope `sslrootcert=system` OUT. Reject it at startup
  with an error explaining that an explicit CA bundle file is required. The
  explicit file path is fully supported on both stacks today. Adding system-store
  support via a libpq ≥16 runtime upgrade is a documented future follow-up.

## D9. diesel-async `custom_setup` closure can hold the config

- **Finding**: `SetupCallback<C> = Box<dyn Fn(&str) -> BoxFuture<ConnectionResult<C>> + Send + Sync>`
  (`diesel-async-0.5.2/src/pooled_connection/mod.rs` ~L46-48). A closure may
  capture by move/clone.
- **Decision**: Build the `rustls::ClientConfig` once, wrap in `Arc`, and
  `Arc::clone` it inside the `custom_setup` closure so TLS config is not rebuilt
  per connection.

## D10. Infra / CA delivery — combined bundle, mounted at deploy (FR-009/009a/009b)

- **Finding (corrected — CRITICAL)**: Prod defaults to **RDS Proxy enabled**
  (`infra/data.tf:118`) and routes `DATABASE_URL` through the **proxy endpoint**
  (`data.tf:135`, url built at `:137`). RDS Proxy presents an **AWS Certificate
  Manager** certificate that chains to **Amazon Trust Services** roots — NOT the
  Amazon RDS CA roots used by a direct instance. So a `verify-full` deployment
  configured with only the RDS CA bundle would **fail the handshake against the
  proxy**. (Direct RDS instance certs DO chain to the RDS CA roots.)
- **Decision (combined bundle)**: The AWS trust anchor is a **combined CA bundle =
  Amazon RDS CA roots + Amazon Trust Services roots**, so `verify-full` succeeds
  against the proxy endpoint (ATS) and a direct instance (RDS CA) alike. Multiple
  roots in one PEM is already supported (rotation-overlap design).
- **Decision (delivery, user-confirmed)**: The published server image stays
  **provider-neutral with NO baked CA** (it runs on non-AWS infra). The combined
  bundle is **mounted/placed at deploy time by the infrastructure** at a fixed
  container path; Terraform sets `sslrootcert` to that path; the app never
  downloads it. Rotation = replace the mounted bundle + restart, no code/image
  change, trust anchor present before mode tightening. (`data.tf` switches the URL
  to `verify-full&sslrootcert=<path>`; `ecs.tf` adds the mount.)
- **No Dockerfile change** — and no `rustls-native-certs` use (system store is out
  of scope).

## D11. Enforcement ordering — preflight before migrations (FR-007/G6)

- **Finding**: Production startup runs migrations before pools
  (`builder/storage.rs:117-119`). The sync libpq migration call does NOT share the
  async resolver. libpq is more permissive: it accepts `allow`/`prefer` and
  connects (possibly over plaintext fallback), and for verify modes with no
  `sslrootcert` it consults `~/.postgresql/root.crt` and emits its own
  un-Guardian-redacted error. So a rejection living only in the async pool path
  would fire *after* an insecure migration connection already happened — both an
  FR-007 divergence and a fail-closed violation.
- **Decision**: Add a **Guardian-level preflight** that parses + validates
  `(sslmode, sslrootcert)` ONCE and rejects `allow`/`prefer`/`system`/unknown and
  verify-without-`sslrootcert`, invoked in `builder/storage.rs` **before**
  `run_migrations`. Both stacks are then governed by the same gate; parity is
  enforced, not hoped for. The parser is exposed (`pub(crate)`) from
  `storage/postgres.rs` and reused by the async path.
- **Implicit libpq sources (parity scope)**: libpq additionally consumes
  `PGSSLROOTCERT`, `PGSSLMODE`, `~/.postgresql/root.crt`, `…/root.crl`, etc. — any
  of which could make the migration stack behave differently from the pool stack
  (e.g. `require` promoted to `verify-ca` because a stray `root.crt` exists).
  Mitigations: (i) for verifying modes Guardian ALWAYS writes an explicit
  `sslrootcert` into the migration URL, so libpq never falls back to the default
  file; (ii) the runtime image carries no `PGSSL*`/dotfile defaults (verified:
  grep of `infra/`, `Dockerfile`, `docs/` found none); (iii) FR-007 parity is
  explicitly **scoped to `DATABASE_URL` inputs within the controlled runtime
  image**. Unifying both consumers on one rustls stack was considered but rejected
  (diesel migrations need a sync libpq connection; no rustls async migration
  harness exists).

## D14. Canonical hostname-verification semantics (verify-full)

- **Finding**: libpq's hostname matching has legacy compatibility behavior
  (Common-Name fallback; particular IP-address handling) that differs from RFC
  6125. rustls/webpki is strict and SAN-based and will reject some certs libpq
  accepts (e.g. CN-only).
- **Decision**: Adopt **strict SAN-based (rustls/webpki) matching** as the single
  canonical contract; CN-only certs are unsupported. Because rustls is the
  stricter side, anything it accepts libpq also accepts — no divergence in the
  accepting direction. Managed providers (RDS, RDS Proxy/ACM, GCP, Azure) issue
  SAN-based certs, so this holds in practice. Cross-stack tests cover DNS-SAN,
  IP-SAN, SAN-mismatch (refused), CN-only (refused). [FR-002a]

## D15. Connection-string parsing rules (deterministic failure)

- **Decision**: Using the `url` crate, define deterministic handling for boundary
  inputs (security-sensitive):
  - duplicate `sslmode` or `sslrootcert` → reject (ambiguous).
  - empty `sslrootcert=` in a verifying mode → reject (missing trust anchor).
  - percent-encoded `sslrootcert` path → decoded before use.
  - libpq **keyword/value DSN** (`host=... sslmode=...`, non-URL) → unsupported →
    reject with a clear error (Guardian uses URL-form `DATABASE_URL`).
  - unsupported scheme / multi-host URL → reject.
  All rejections happen in the preflight, before any connection. [data-model.md]

## D12. Absent `sslmode` default divergence

- **Finding**: libpq defaults absent `sslmode` to `prefer` (TLS-first, plaintext
  fallback). The async path treats absent as plaintext (`NoTls`). Against a
  non-TLS local server both end up plaintext (FR-006 fine), but against a
  TLS-capable server they diverge (libpq encrypts, async stays plaintext) — the
  async side is more permissive, contradicting G6.
- **Decision**: The preflight **normalizes absent `sslmode` to `disable` on both
  stacks** by injecting `sslmode=disable` into the URL handed to the sync
  migration path (the async path already treats absent as `NoTls`). This removes
  the divergence and keeps local plaintext dev working. Operators who want TLS
  MUST set an explicit `sslmode`. Documented as an explicit, fail-safe
  normalization (least surprise; you opt into TLS deliberately).

---

### Resolved unknowns

| Spec marker | Resolution |
|---|---|
| FR-001b (`allow`/`prefer`) | Reject at startup (D7) |
| FR-001c (`require`+rootcert) | Follow libpq → verify-ca (D6) |
| FR-003 / FR-003a (system store) | Out of scope; explicit CA file only (D8) |
| Trust-anchor config surface | `sslrootcert` in `DATABASE_URL` (D1) |
| Async-stack mode support | Parse + strip; rustls verifier enforces level (D2) |
| FR-007/G6 enforcement | Shared preflight before migrations (D11) |
| Absent `sslmode` parity | Normalize to `disable` on both stacks (D12) |

No NEEDS CLARIFICATION markers remain.
