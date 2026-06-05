# Contract: Database TLS Verification Behavior

This feature changes no HTTP/gRPC API. Its externally observable contract is the
mapping from standard libpq connection-string parameters to connection behavior,
which MUST hold identically on both the sync migration stack and the async pool
stack.

## Inputs (from `DATABASE_URL`)

- `sslmode` ∈ `{disable, allow, prefer, require, verify-ca, verify-full}` (absent ⇒ treated as no-TLS)
- `sslrootcert` = `<filesystem path>` | `system` | absent

## Behavior matrix (authoritative)

| `sslmode` | `sslrootcert` | Connection behavior | On verification failure | On misconfig |
|---|---|---|---|---|
| absent | (any) | plaintext, no TLS (normalized to `disable` on BOTH stacks; libpq's default `prefer` is overridden so the stacks don't diverge) | — | — |
| `disable` | (any) | plaintext, no TLS | — | — |
| `allow` | (any) | **rejected at startup** | — | error: use explicit mode |
| `prefer` | (any) | **rejected at startup** | — | error: use explicit mode |
| `require` | absent | TLS, encrypt-only (no cert check) | — | — |
| `require` | `<path>` | TLS + chain verified (promoted to verify-ca) | connection refused | error if path bad |
| `verify-ca` | `<path>` | TLS + chain verified (hostname NOT checked) | connection refused | error if path bad |
| `verify-ca` | absent | **rejected at startup** | — | error: trust anchor required |
| `verify-full` | `<path>` | TLS + chain + hostname verified | connection refused | error if path bad |
| `verify-full` | absent | **rejected at startup** | — | error: trust anchor required |
| (any) | `system` | **rejected at startup** | — | error: unsupported, use explicit CA file |
| unknown `sslmode` | (any) | **rejected at startup** | — | error: unrecognized sslmode |

## Guarantees

- **G1**: In `verify-ca` and `verify-full`, a server certificate that does not
  chain to the configured CA bundle, or is expired/not-yet-valid, causes the
  connection to be **refused** (never accepted). [SC-001, FR-002]
- **G2**: In `verify-full` ONLY, a hostname mismatch causes refusal. Hostname
  matching is **strict SAN-based** (RFC 6125-style; the canonical semantics — NOT
  libpq's CN fallback). A certificate without a matching Subject Alternative Name
  (including CN-only certs) is refused. In `verify-ca`, hostname is not checked.
  [SC-001, FR-002, FR-002a]
- **G3**: No code path accepts an unverified certificate in a verifying mode; the
  only non-verifying verifier is reachable solely via `require` without
  `sslrootcert` (encrypt-only). [FR-004]
- **G4**: Any misconfiguration in a verifying mode (missing/unreadable/empty CA
  bundle, `system`, `allow`/`prefer`, unknown `sslmode`) **fails fast** via a
  shared preflight that runs **before migrations** (`builder/storage.rs`, ahead of
  `run_migrations`); never a silent downgrade and never an insecure migration
  connection slipping through ahead of the rejection. [FR-005]
- **G5**: Errors and logs keep the database password and secret query parameters
  redacted. [FR-005a]
- **G6**: The sync migration stack and the async pool stack produce the **same**
  behavior for the same inputs — enforced by the shared preflight (incl. the
  absent-`sslmode`→`disable` normalization), not by independent agreement between
  libpq and rustls. [FR-007]

## Verification (how each row is exercised)

- Parser/resolver unit tests: every matrix row maps raw input → expected
  effective level or rejection (pure, no network). Includes parsing edge cases —
  duplicate `sslmode`/`sslrootcert`, empty `sslrootcert=`, percent-encoded path,
  non-URL keyword/value DSN, unsupported scheme, multi-host — all rejected
  deterministically.
- CA-loader unit test: a bundle with any malformed PEM entry is rejected (entire
  bundle must parse; no silent drop).
- P3 plaintext/encrypt-only (FR-006), on BOTH paths: omitted `sslmode` →
  plaintext; `disable` → plaintext; `require` (no rootcert) → TLS without
  verification; `require` → refuses a non-TLS server.
- Local TLS Postgres (automated/scripted): exercise `verify-ca` and `verify-full`
  success + failure (untrusted CA, expired cert, hostname mismatch). Hostname
  matrix: DNS-SAN match, IP-SAN match, SAN mismatch (refused), CN-only (refused).
- Manual smoke (AGENTS.md §6) against AWS RDS and one other managed provider:
  `verify-full` success + a deliberately wrong CA refusal. For AWS this MUST be
  run against the **RDS Proxy endpoint** (prod default) AND a direct-instance
  endpoint, using the **combined CA bundle** (RDS roots + Amazon Trust Services
  roots) — the proxy presents an ACM cert chaining to ATS, the direct instance
  chains to the RDS CA roots; both must succeed under `verify-full` with the one
  combined bundle.
- Preflight ordering: a test/check confirms `allow`/`prefer`/`system`/unknown and
  verify-without-`sslrootcert` are rejected **before** any migration connection is
  attempted (no insecure connection precedes the rejection).
- Redaction test: a forced verifying-mode failure asserts the error/log contains
  no password.
