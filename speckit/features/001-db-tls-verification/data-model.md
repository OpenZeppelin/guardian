# Phase 1 Data Model: TLS Verification Configuration

This feature introduces **no database schema** and **no persisted entities**. The
"model" here is the in-memory configuration parsed from the connection string and
the verifier it selects. Records, deltas, proposals, and migrations are unchanged.

## Entities (in-memory, per pool)

### TlsVerificationLevel (resolved)

The effective level after parsing + libpq normalization. Closed set:

| Variant | Meaning | Trust anchor required |
|---|---|---|
| `NoTls` | plaintext connection | no |
| `EncryptOnly` | TLS, no certificate verification | no |
| `VerifyCa` | TLS + certificate chain validated | yes (CA bundle file) |
| `VerifyFull` | TLS + chain + hostname (SAN) validated | yes (CA bundle file) |

Derived from raw `sslmode` + presence of `sslrootcert` (see Resolution rules).

### TrustAnchor

- **Source**: explicit CA bundle file at the `sslrootcert=<path>` location. For
  the AWS reference this is a **combined bundle** (Amazon RDS roots + Amazon Trust
  Services roots) so it validates both the RDS Proxy (ACM/ATS) and direct-instance
  (RDS CA) endpoints.
- **Attributes**: filesystem path; loaded into a `rustls::RootCertStore`
  containing ≥1 certificate (multiple allowed for rotation overlap / multi-root).
- **Invalid states (fail fast)**: path missing, unreadable, empty, OR containing
  **any** malformed PEM entry, while a verifying level is selected. The ENTIRE
  bundle must parse — no partial loads, no silently-dropped certs.
- **Unsupported**: `sslrootcert=system` (rejected — out of scope this feature).

### ParsedConnectionConfig (three explicit URL values)

The product of parsing `DATABASE_URL`. Modeled as three distinct values so the
"untouched vs normalized" ambiguity cannot recur:
- `raw_sslmode`: one of `disable|allow|prefer|require|verify-ca|verify-full` or
  absent/unknown.
- `sslrootcert`: optional path; empty `sslrootcert=` and the literal `system` are
  both recognized-and-rejected (the latter as unsupported).
- `raw_url`: the operator's original string (parsed, never connected with directly).
- `normalized_sync_url`: for libpq/migrations — absent `sslmode` → `disable`
  injected; for verifying modes an **explicit `sslrootcert=<path>` always present**
  (no implicit `~/.postgresql/root.crt` fallback). Otherwise carries the standard
  libpq tokens unchanged.
- `sanitized_async_url`: for tokio-postgres — `sslrootcert` removed, `sslmode`
  reduced to `require` (force TLS) or `disable`; the rustls verifier enforces the
  real level.

### Parsing rules (deterministic failure)

| Input | Behavior |
|---|---|
| duplicate `sslmode` / `sslrootcert` | reject (ambiguous) |
| empty `sslrootcert=` in a verifying mode | reject (missing trust anchor) |
| percent-encoded `sslrootcert` path | decode before use |
| libpq keyword/value DSN (`host=… sslmode=…`) | reject — URL form only |
| unsupported scheme / multi-host URL | reject |

All parsing rejections occur in the preflight, before any connection attempt.

## Resolution rules (raw → effective)

```
absent                                   -> NoTls (normalized: inject sslmode=disable
                                            into the sync/migration URL too, so libpq's
                                            default `prefer` does not diverge — see D12)
disable                                  -> NoTls
allow | prefer                           -> REJECT (fail fast)
unknown sslmode value                    -> REJECT (fail fast)
require        + no sslrootcert           -> EncryptOnly
require        + sslrootcert=<path>       -> VerifyCa        (libpq promotion)
verify-ca      + sslrootcert=<path>       -> VerifyCa
verify-full    + sslrootcert=<path>       -> VerifyFull
verify-ca / verify-full + no sslrootcert  -> REJECT (fail fast: trust anchor required)
any            + sslrootcert=system       -> REJECT (fail fast: unsupported)
```

## Verifier selection (effective level → rustls)

| Effective level | rustls construction |
|---|---|
| `NoTls` | no TLS connector; plain diesel-async manager (no `custom_setup`) |
| `EncryptOnly` | `ClientConfig` with a no-verify `ServerCertVerifier` (the ONLY non-verifying verifier; legitimate because this is not a verifying mode — FR-004) |
| `VerifyCa` | `ClientConfig` with custom chain-only verifier delegating to `WebPkiServerVerifier(root_store)`, hostname mismatch tolerated |
| `VerifyFull` | `ClientConfig::builder().with_root_certificates(root_store).with_no_client_auth()` (default WebPki verifier; chain + hostname) |

## Cross-stack consistency (FR-007)

| Stack | How it reads the model |
|---|---|
| Sync migrations (libpq via `PgConnection::establish`) | `normalized_sync_url` (absent→`disable`, explicit `sslrootcert` for verifying modes); libpq applies `sslmode`/`sslrootcert` natively, incl. the `require`+rootcert promotion. Implicit `PGSSL*`/`~/.postgresql/root.crt` neutralized (controlled image). |
| Async pools (tokio-postgres + rustls) | parsed config drives verifier selection; `sanitized_async_url` passed to `tokio_postgres::connect` |

Both stacks resolve the **same** `(sslmode, sslrootcert)` inputs to the **same**
effective level. A mismatch (one verifying, one not) is a defect.

**Parity is enforced by a shared preflight, not by coincidence.** Because libpq is
more permissive than Guardian's resolver (accepts `allow`/`prefer`, defaults
absent `sslmode` to `prefer`, falls back to `~/.postgresql/root.crt`), a
Guardian-level preflight parses + validates `(sslmode, sslrootcert)` ONCE and
rejects the unsupported combinations **before `run_migrations`** runs
(`builder/storage.rs:117-119` runs migrations first). The same preflight performs
the absent-`sslmode`→`disable` normalization for the migration URL. Without this
gate the sync stack could connect (insecurely) before the async stack's rejection.

## Lifecycle

The preflight runs first, then configuration is resolved **once at startup / pool
creation** and is immutable for the process lifetime. The `rustls::ClientConfig` is built once, `Arc`-wrapped, and
shared into every async connection setup. CA rotation = replace the bundle file +
restart (no hot reload).
